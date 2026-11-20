//! A table of C signatures, generated from a seed, checked against clang
//! both ways: nabi calls C (directly, through an `extern "C" fn` value,
//! with a variadic tail) and C calls nabi `extern "C" fn`s. Each callee
//! checksums the leaves it gets against the generator's sum and returns
//! a constant its caller compares leaf by leaf.
//!
//! The types: integers of each width, `bool`, `c_char`, `f32`, `f64`,
//! `f16` / `bf16` and `Simd` where the target's C has them, structs
//! (nested, arrays, packed, over-aligned), unions, homogeneous float
//! aggregates; the signatures run the registers out and return nothing,
//! scalars, small and large aggregates. A fixed part comes first, the
//! shapes calling conventions single out (`table`); `NABI_ABI_CASES`
//! (default 160) random ones follow, from `NABI_ABI_SEED`.
//!
//! On a target whose C has no `_Float16` or vectors, the refusal is
//! checked instead (`unsupported_types_are_refused`).
#![allow(clippy::many_single_char_names, clippy::cognitive_complexity)]

use crate::common::Rng;
use crate::native::ffi::c_program;
#[allow(clippy::wildcard_imports)]
use crate::native::*;
use std::fmt::Write as _;

const SEED: u64 = 0x00ab_1cab_1e5e_ed01;
const FNV: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0100_0000_01b3;

/// A scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sc {
    /// Bits, signed.
    I(u32, bool),
    Bool,
    Char,
    F32,
    F64,
    F16,
    BF16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ty {
    Sc(Sc),
    /// A `Simd` of this element and lane count.
    Vec(Sc, u32),
    /// An aggregate of the table (`Gen::aggs`).
    Agg(usize),
}

#[derive(Clone, Debug)]
struct Agg {
    union: bool,
    packed: bool,
    /// `align(N)`; 0 without.
    align: u32,
    /// A field and its array length (0: no array).
    fields: Vec<(Ty, u32)>,
}

/// What the target's C passes.
#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
struct Feats {
    f16: bool,
    bf16: bool,
    /// An `f16` / `bf16` result.
    half_ret: bool,
    simd: bool,
    /// `extern "system"` is stdcall.
    stdcall: bool,
    /// `repr(align(16))` aggregates by value (not to i686 Windows cdecl
    /// functions, an error there).
    align16: bool,
    /// The ABI alignment of 8-byte scalars.
    wide: u32,
    /// The C is MSVC's: `__declspec` and `#pragma pack`, no GNU
    /// extensions.
    msvc: bool,
    /// `__bf16` inside aggregates: gcc on `AArch64` passes them as
    /// integers, against AAPCS64 (and clang), which nabi follows; gcc on
    /// 32-bit Arm passes them in VFP registers, against AAPCS32 (a
    /// homogeneous aggregate of half-precision floats is no VFP
    /// candidate; clang agrees).
    bf16_agg: bool,
    /// A 1, 2, 4 or 8-byte aggregate result with a member of another
    /// size (`char[3]`) in `eax` / `edx:eax`: clang-cl on i686 returns
    /// it through a hidden pointer, against MSVC (`cl`), which nabi
    /// follows.
    odd_small_ret: bool,
    /// Stack arguments below 8 bytes on Apple arm64: gcc packs an
    /// aggregate at its own alignment and size, where clang rounds both
    /// up to 8 (AAPCS64's rule, which Apple keeps for aggregates), and
    /// starts a variadic tail right after the named arguments, not at
    /// the next 8 bytes; nabi follows clang.
    packed_stack_agg: bool,
}

impl Feats {
    fn of(t: &nabic_target::TargetSpec) -> Self {
        let arch = t.arch.as_str();
        let half = matches!(
            arch,
            "aarch64" | "x86_64" | "x86" | "arm" | "riscv64" | "riscv32" | "s390x"
        );
        Self {
            f16: half,
            bf16: half && arch != "s390x",
            half_ret: half && arch != "x86",
            simd: matches!(arch, "aarch64" | "x86_64"),
            stdcall: arch == "x86" && t.os == "windows",
            align16: !(arch == "x86" && t.os == "windows"),
            wide: t.wide_align,
            msvc: false,
            bf16_agg: true,
            odd_small_ret: true,
            packed_stack_agg: true,
        }
    }
}

/// One signature.
#[derive(Clone, Debug)]
struct Case {
    params: Vec<Ty>,
    /// `None`: no result.
    ret: Option<Ty>,
    /// Scalars after a C `...` (nabi calls C only).
    extra: Vec<Sc>,
    /// `extern "system"`.
    system: bool,
}

struct Gen {
    aggs: Vec<Agg>,
    feats: Feats,
    rng: Rng,
}

/// A leaf of a value: its path from the value (`.f0[2]`, a vector lane
/// `[1]`), type and bits.
struct Leaf {
    path: String,
    /// A vector lane: `Some(lane)`, the path naming the vector.
    lane: Option<u32>,
    sc: Sc,
    bits: u64,
}

impl Sc {
    const fn size(self, wide: u32) -> (u32, u32) {
        match self {
            Self::I(b, _) => {
                let s = b / 8;
                (s, if s == 8 { wide } else { s })
            }
            Self::Bool | Self::Char => (1, 1),
            Self::F32 => (4, 4),
            Self::F64 => (8, wide),
            Self::F16 | Self::BF16 => (2, 2),
        }
    }

    fn nabi(self) -> String {
        match self {
            Self::I(b, true) => format!("i{b}"),
            Self::I(b, false) => format!("u{b}"),
            Self::Bool => "bool".into(),
            Self::Char => "c_char".into(),
            Self::F32 => "f32".into(),
            Self::F64 => "f64".into(),
            Self::F16 => "f16".into(),
            Self::BF16 => "bf16".into(),
        }
    }

    const fn c(self) -> &'static str {
        match self {
            Self::I(8, true) => "signed char",
            Self::I(8, false) => "unsigned char",
            Self::I(16, true) => "short",
            Self::I(16, false) => "unsigned short",
            Self::I(32, true) => "int",
            Self::I(32, false) => "unsigned",
            Self::I(_, true) => "long long",
            Self::I(_, false) => "unsigned long long",
            Self::Bool => "_Bool",
            Self::Char => "char",
            Self::F32 => "float",
            Self::F64 => "double",
            Self::F16 => "_Float16",
            Self::BF16 => "__bf16",
        }
    }

    /// Random bits of a value: floats normal and finite, so no x87 move
    /// changes them.
    fn bits(self, rng: &mut Rng) -> u64 {
        let r = rng.next();
        match self {
            Self::I(64, _) => r,
            Self::I(b, _) => r & ((1 << b) - 1),
            Self::Bool => r & 1,
            Self::Char => 0x41 + r % 26,
            Self::F32 => {
                (r & 0x8000_0000) | ((0x70 + (r >> 8) % 0x20) << 23) | ((r >> 40) & 0x7f_ffff)
            }
            Self::F64 => {
                (r & (1 << 63))
                    | ((0x3e0 + (r >> 8) % 0x40) << 52)
                    | ((r >> 12) & 0xf_ffff_ffff_ffff)
            }
            Self::F16 => (r & 0x8000) | ((8 + (r >> 8) % 16) << 10) | ((r >> 20) & 0x3ff),
            Self::BF16 => (r & 0x8000) | ((0x70 + (r >> 8) % 0x20) << 7) | ((r >> 20) & 0x7f),
        }
    }

    fn nabi_lit(self, bits: u64) -> String {
        match self {
            Self::I(b, false) => format!("{bits}u{b}"),
            Self::I(b, true) => format!("({bits}u{b} as i{b})"),
            Self::Bool => (bits != 0).to_string(),
            Self::Char => format!("c'{}'", char::from(bits as u8)),
            Self::F32 => format!("f32::from_bits({bits}u32)"),
            Self::F64 => format!("f64::from_bits({bits}u64)"),
            Self::F16 => format!("f16::from_bits({bits}u16)"),
            Self::BF16 => format!("bf16::from_bits({bits}u16)"),
        }
    }

    fn c_lit(self, bits: u64) -> String {
        match self {
            Self::I(..) => format!("({})0x{bits:x}ull", self.c()),
            Self::Bool => bits.to_string(),
            Self::Char => format!("'{}'", char::from(bits as u8)),
            Self::F32 => format!("fv(0x{bits:x}u)"),
            Self::F64 => format!("dv(0x{bits:x}ull)"),
            Self::F16 => format!("hv(0x{bits:x})"),
            Self::BF16 => format!("bv(0x{bits:x})"),
        }
    }

    /// `e`'s bits as a `u64`, in nabi.
    fn nabi_bits(self, e: &str) -> String {
        match self {
            Self::I(64, false) => format!("({e})"),
            Self::I(64, true) => format!("(({e}) as u64)"),
            Self::I(b, _) => format!("(({e}) as u{b} as u64)"),
            Self::Bool => format!("(if {e} {{ 1u64 }} else {{ 0u64 }})"),
            Self::Char => format!("(({e}) as u8 as u64)"),
            Self::F64 => format!("(({e}).to_bits())"),
            _ => format!("(({e}).to_bits() as u64)"),
        }
    }

    fn c_bits(self, e: &str) -> String {
        match self {
            Self::I(8, _) | Self::Char => format!("(u64)(unsigned char)({e})"),
            Self::I(16, _) => format!("(u64)(unsigned short)({e})"),
            Self::I(32, _) => format!("(u64)(unsigned)({e})"),
            Self::I(..) | Self::Bool => format!("(u64)({e})"),
            Self::F32 => format!("fb({e})"),
            Self::F64 => format!("db({e})"),
            Self::F16 => format!("hb({e})"),
            Self::BF16 => format!("bb({e})"),
        }
    }
}

const fn mix(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(PRIME)
}

impl Gen {
    const fn new(feats: Feats, seed: u64) -> Self {
        Self {
            aggs: Vec::new(),
            feats,
            rng: Rng(seed),
        }
    }

    fn size_align(&self, t: &Ty) -> (u32, u32) {
        let w = self.feats.wide;
        match t {
            Ty::Sc(s) => s.size(w),
            Ty::Vec(e, n) => {
                let s = e.size(w).0 * n;
                (s, s)
            }
            Ty::Agg(i) => {
                let a = &self.aggs[*i];
                let (mut size, mut align) = (0u32, 1u32);
                for (f, n) in &a.fields {
                    let (s, fa) = self.size_align(f);
                    let s = s * (*n).max(1);
                    let fa = if a.packed { 1 } else { fa };
                    align = align.max(fa);
                    if a.union {
                        size = size.max(s);
                    } else {
                        size = size.next_multiple_of(fa) + s;
                    }
                }
                let align = align.max(a.align);
                (size.next_multiple_of(align), align)
            }
        }
    }

    fn scalar(&mut self) -> Sc {
        let mut v = vec![
            Sc::I(8, true),
            Sc::I(8, false),
            Sc::I(16, true),
            Sc::I(16, false),
            Sc::I(32, true),
            Sc::I(32, false),
            Sc::I(64, true),
            Sc::I(64, false),
            Sc::Bool,
            Sc::Char,
            Sc::F32,
            Sc::F32,
            Sc::F64,
            Sc::F64,
        ];
        if self.feats.f16 {
            v.push(Sc::F16);
        }
        if self.feats.bf16 {
            v.push(Sc::BF16);
        }
        *self.rng.pick(&v)
    }

    fn float(&mut self) -> Sc {
        let mut v = vec![Sc::F32, Sc::F64];
        if self.feats.f16 {
            v.push(Sc::F16);
        }
        *self.rng.pick(&v)
    }

    fn agg(&mut self, a: Agg) -> Ty {
        self.aggs.push(a);
        Ty::Agg(self.aggs.len() - 1)
    }

    /// A random aggregate, `depth` deep at most.
    fn rand_agg(&mut self, depth: u32) -> Ty {
        let homog = self.rng.chance(30);
        let base = self.float();
        let n = 1 + self.rng.below(if homog { 4 } else { 5 });
        let mut fields = Vec::new();
        for _ in 0..n {
            let len = if self.rng.chance(15) {
                1 + self.rng.below(4) as u32
            } else {
                0
            };
            let t = if homog {
                Ty::Sc(base)
            } else if depth > 0 && self.rng.chance(20) {
                self.rand_agg(depth - 1)
            } else {
                let mut s = self.scalar();
                while s == Sc::BF16 && !self.feats.bf16_agg {
                    s = self.scalar();
                }
                Ty::Sc(s)
            };
            fields.push((t, len));
        }
        let union = self.rng.chance(15);
        let packed = !union && self.rng.chance(8);
        let align = if !packed && !union && self.feats.align16 && self.rng.chance(6) {
            16
        } else {
            0
        };
        if union {
            // The first member is the largest: it is the one written.
            fields.sort_by_key(|(t, n)| std::cmp::Reverse(self.size_align(t).0 * (*n).max(1)));
        }
        self.agg(Agg {
            union,
            packed,
            align,
            fields,
        })
    }

    fn vector(&mut self) -> Ty {
        let (e, n) = *self.rng.pick(&[
            (Sc::F32, 4),
            (Sc::F32, 2),
            (Sc::F64, 2),
            (Sc::I(32, true), 4),
            (Sc::I(8, false), 16),
            (Sc::I(16, true), 8),
            (Sc::I(64, false), 2),
            (Sc::I(32, false), 2),
        ]);
        Ty::Vec(e, n)
    }

    fn rand_ty(&mut self) -> Ty {
        let r = self.rng.below(100);
        if r < 45 {
            Ty::Sc(self.scalar())
        } else if r < 52 && self.feats.simd {
            self.vector()
        } else {
            self.rand_agg(2)
        }
    }

    fn rand_case(&mut self) -> Case {
        let max = if self.rng.chance(20) { 14 } else { 6 };
        let n = self.rng.below(max);
        let params = (0..n).map(|_| self.rand_ty()).collect();
        let ret = if self.rng.chance(15) {
            None
        } else {
            Some(self.rand_ty())
        };
        let extra = if self.rng.chance(12) {
            let k = 1 + self.rng.below(5);
            (0..k)
                .map(|_| {
                    *self.rng.pick(&[
                        Sc::I(32, true),
                        Sc::I(64, true),
                        Sc::F64,
                        Sc::F32,
                        Sc::I(8, true),
                        Sc::I(16, false),
                    ])
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut c = Case {
            params,
            ret,
            extra,
            system: self.feats.stdcall && self.rng.chance(15),
        };
        self.legal(&mut c);
        c
    }

    /// A register-sized type all of whose members are: clang's rule for
    /// a small aggregate result in registers on i686.
    fn reg_sized(&self, t: &Ty) -> bool {
        let reg = |s: u32| matches!(s, 1 | 2 | 4 | 8);
        if !reg(self.size_align(t).0) {
            return false;
        }
        match t {
            Ty::Agg(i) => self.aggs[*i]
                .fields
                .iter()
                .all(|(f, n)| (*n == 0 || reg(self.size_align(f).0 * n)) && self.reg_sized(f)),
            _ => true,
        }
    }

    /// What the target cannot take: a half result on i686, a variadic
    /// function without a fixed parameter or with stdcall.
    fn legal(&self, c: &mut Case) {
        if !self.feats.half_ret && matches!(c.ret, Some(Ty::Sc(Sc::F16 | Sc::BF16))) {
            c.ret = Some(Ty::Sc(Sc::F32));
        }
        if let Some(t @ Ty::Agg(_)) = &c.ret {
            let small = matches!(self.size_align(t).0, 1 | 2 | 4 | 8);
            if !self.feats.odd_small_ret && small && !self.reg_sized(t) {
                c.ret = Some(Ty::Sc(Sc::I(64, true)));
            }
        }
        if !c.extra.is_empty() {
            c.system = false;
            // `va_start` names a parameter no promotion changes.
            if !matches!(c.params.last(), Some(Ty::Sc(Sc::I(32 | 64, _) | Sc::F64))) {
                c.params.push(Ty::Sc(Sc::I(32, true)));
            }
        }
        if !self.feats.packed_stack_agg {
            self.no_packed_stack_agg(c);
        }
    }

    /// The base and count of a homogeneous float aggregate (no vectors
    /// inside), as clang finds it: a union takes its largest member.
    fn hfa(&self, t: &Ty) -> Option<(Sc, u32)> {
        let Ty::Agg(i) = t else {
            return match t {
                Ty::Sc(s @ (Sc::F16 | Sc::BF16 | Sc::F32 | Sc::F64)) => Some((*s, 1)),
                _ => None,
            };
        };
        let a = &self.aggs[*i];
        let (mut base, mut count) = (None, 0);
        for (f, n) in &a.fields {
            let (b, k) = self.hfa(f)?;
            if base.is_some_and(|x| x != b) {
                return None;
            }
            base = Some(b);
            let k = k * (*n).max(1);
            count = if a.union { count.max(k) } else { count + k };
        }
        let b = base?;
        (self.size_align(t).0 == count * b.size(self.feats.wide).0).then_some((b, count))
    }

    /// What AAPCS64 puts on the stack below 8 bytes of alignment or size
    /// becomes an `i64`: an aggregate, and in a variadic function any
    /// named argument (`packed_stack_agg`).
    fn no_packed_stack_agg(&self, c: &mut Case) {
        let variadic = !c.extra.is_empty();
        let (mut gr, mut fr) = (0u32, 0u32);
        for p in &mut c.params {
            let (size, align) = self.size_align(p);
            let hfa = self.hfa(p).filter(|h| h.1 <= 4);
            let (regs, n) = match p {
                Ty::Vec(..) => (&mut fr, 1),
                _ if hfa.is_some() => (&mut fr, hfa.map_or(1, |h| h.1)),
                // Larger aggregates go by reference.
                Ty::Agg(_) if size <= 16 => (&mut gr, size.div_ceil(8)),
                _ => (&mut gr, 1),
            };
            let fits = *regs + n <= 8;
            *regs = if fits { *regs + n } else { 8 };
            let by_ref = matches!(p, Ty::Agg(_)) && hfa.is_none() && size > 16;
            let small = align < 8 || size % 8 != 0;
            if !fits && !by_ref && small && (variadic || matches!(p, Ty::Agg(_))) {
                *p = Ty::Sc(Sc::I(64, true));
            }
        }
    }

    /// The shapes conventions single out.
    fn table(&mut self) -> Vec<Case> {
        let f = |s| Ty::Sc(s);
        let i32_ = Sc::I(32, true);
        let i64_ = Sc::I(64, true);
        let mut out = Vec::new();
        let one = |out: &mut Vec<Case>, params: Vec<Ty>, ret: Option<Ty>| {
            out.push(Case {
                params,
                ret,
                extra: Vec::new(),
                system: false,
            });
        };
        let st = |g: &mut Self, fields: Vec<(Ty, u32)>| {
            g.agg(Agg {
                union: false,
                packed: false,
                align: 0,
                fields,
            })
        };
        let un = |g: &mut Self, fields: Vec<(Ty, u32)>| {
            g.agg(Agg {
                union: true,
                packed: false,
                align: 0,
                fields,
            })
        };
        // Unions by value: of floats (an HFA, SSE), of a double and a
        // float, of an int and a float, of float arrays.
        let uf = un(self, vec![(f(Sc::F32), 0), (f(Sc::F32), 0)]);
        let ud = un(self, vec![(f(Sc::F64), 0), (f(Sc::F32), 0)]);
        let ui = un(self, vec![(f(i32_), 0), (f(Sc::F32), 0)]);
        let ua = un(self, vec![(f(Sc::F32), 4), (f(Sc::F32), 2)]);
        let ul = un(self, vec![(f(Sc::F64), 2), (f(i64_), 0)]);
        let pair = st(self, vec![(f(Sc::F32), 0), (f(Sc::F32), 0)]);
        let un2 = un(self, vec![(pair.clone(), 0), (f(Sc::F64), 0)]);
        let in_un = st(self, vec![(f(i32_), 0), (uf.clone(), 0)]);
        for u in [&uf, &ud, &ui, &ua, &ul, &un2, &in_un] {
            one(&mut out, vec![u.clone()], Some(u.clone()));
            one(
                &mut out,
                vec![f(i32_), u.clone(), f(Sc::F64), u.clone()],
                Some(u.clone()),
            );
        }
        // HFAs of four floats and in nested arrays; a 16-aligned
        // aggregate after an odd register; registers run out.
        let hfa4 = st(self, vec![(f(Sc::F32), 4)]);
        let nested = st(self, vec![(f(Sc::F32), 2), (pair.clone(), 0)]);
        let d4 = st(self, vec![(f(Sc::F64), 0), (f(Sc::F64), 3)]);
        let a16 = self.agg(Agg {
            union: false,
            packed: false,
            align: if self.feats.align16 { 16 } else { 0 },
            fields: vec![(f(i64_), 0), (f(i64_), 0)],
        });
        one(&mut out, vec![hfa4.clone(), nested.clone()], Some(nested));
        one(&mut out, vec![d4.clone(), d4.clone()], Some(d4));
        one(
            &mut out,
            vec![f(i32_), a16.clone(), f(i32_), a16.clone()],
            Some(a16.clone()),
        );
        one(
            &mut out,
            vec![f(Sc::F64); 9]
                .into_iter()
                .chain([hfa4.clone(), f(Sc::F32)])
                .collect(),
            Some(hfa4.clone()),
        );
        one(
            &mut out,
            vec![f(i64_); 7]
                .into_iter()
                .chain([a16, pair.clone()])
                .collect(),
            None,
        );
        // Mixed float kinds; a byval of 68 bytes; 3 bytes; padding.
        let big = st(self, vec![(f(i32_), 17)]);
        let three = st(self, vec![(f(Sc::I(8, true)), 3)]);
        let mixed = st(
            self,
            vec![(f(Sc::F32), 0), (f(Sc::F64), 0), (f(Sc::F32), 0)],
        );
        let fd = st(
            self,
            vec![
                (f(Sc::F32), 0),
                (f(i32_), 0),
                (f(Sc::F32), 0),
                (f(Sc::F64), 0),
            ],
        );
        one(
            &mut out,
            vec![f(Sc::F32), big.clone(), f(Sc::F64), three.clone()],
            Some(big.clone()),
        );
        one(
            &mut out,
            vec![mixed.clone(), fd.clone(), three.clone()],
            Some(three.clone()),
        );
        one(
            &mut out,
            vec![f(Sc::F32), f(Sc::F64), f(Sc::F32), mixed],
            Some(fd),
        );
        // ELFv2's HFA of five to eight members.
        let f8 = st(self, vec![(f(Sc::F32), 8)]);
        let d5 = st(self, vec![(f(Sc::F64), 5)]);
        one(&mut out, vec![f8.clone(), d5.clone()], Some(f8));
        one(&mut out, vec![d5.clone()], Some(d5));
        // Results not extended by the callee everywhere.
        for s in [
            Sc::Bool,
            Sc::Char,
            Sc::I(8, true),
            Sc::I(16, false),
            Sc::I(8, false),
        ] {
            one(&mut out, vec![f(s), f(i32_)], Some(f(s)));
        }
        if self.feats.f16 {
            let h2 = st(self, vec![(f(Sc::F16), 0), (f(Sc::F16), 0)]);
            let h3 = st(self, vec![(f(Sc::F16), 3)]);
            let hf = st(self, vec![(f(Sc::F16), 0), (f(Sc::F32), 0)]);
            let hi = st(self, vec![(f(Sc::F16), 0), (f(i32_), 0)]);
            let uh = un(self, vec![(f(Sc::F32), 0), (f(Sc::F16), 0)]);
            let ret = |s| {
                if self.feats.half_ret {
                    Some(f(s))
                } else {
                    Some(f(Sc::F32))
                }
            };
            one(&mut out, vec![f(Sc::F16), f(Sc::F16)], ret(Sc::F16));
            one(
                &mut out,
                vec![f(Sc::F64); 8]
                    .into_iter()
                    .chain([f(Sc::F16), f(i32_), f(Sc::F16)])
                    .collect(),
                ret(Sc::F16),
            );
            for a in [&h2, &h3, &hf, &hi, &uh] {
                one(&mut out, vec![a.clone(), f(Sc::F16)], Some(a.clone()));
            }
            if self.feats.bf16 {
                one(
                    &mut out,
                    vec![f(Sc::BF16), f(Sc::F32), f(Sc::BF16)],
                    ret(Sc::BF16),
                );
            }
        }
        if self.feats.simd {
            let v4 = Ty::Vec(Sc::F32, 4);
            let v2 = Ty::Vec(Sc::F32, 2);
            let d2 = Ty::Vec(Sc::F64, 2);
            let i4 = Ty::Vec(i32_, 4);
            let b16 = Ty::Vec(Sc::I(8, false), 16);
            one(&mut out, vec![v4.clone(), v4.clone()], Some(v4.clone()));
            one(&mut out, vec![v2.clone(), f(Sc::F32), v2.clone()], Some(v2));
            one(
                &mut out,
                vec![d2.clone(), i4.clone(), b16.clone()],
                Some(i4),
            );
            one(&mut out, vec![v4.clone(); 10], Some(v4.clone()));
            one(&mut out, vec![f(Sc::F64), v4, f(i32_), d2, hfa4], Some(b16));
        }
        if self.feats.stdcall {
            let dt = st(self, vec![(f(i32_), 0), (f(i32_), 0)]);
            for c in [
                (vec![f(i32_), pair.clone()], Some(dt.clone())),
                (vec![dt, f(Sc::F64)], Some(f(Sc::F64))),
                (vec![big, f(Sc::I(16, true))], Some(three)),
            ] {
                out.push(Case {
                    params: c.0,
                    ret: c.1,
                    extra: Vec::new(),
                    system: true,
                });
            }
        }
        // Variadic tails: promoted narrow integers and floats, after a
        // struct and after the registers run out.
        for (params, extra) in [
            (
                vec![f(i32_)],
                vec![Sc::F64, Sc::I(32, true), Sc::F32, Sc::I(64, true)],
            ),
            (
                vec![pair, f(i32_)],
                vec![Sc::I(8, true), Sc::F64, Sc::I(16, false)],
            ),
            (vec![f(Sc::F64); 9], vec![Sc::F64, Sc::F64, Sc::I(64, true)]),
            (vec![f(i64_); 8], vec![Sc::I(32, true), Sc::F64]),
        ] {
            out.push(Case {
                params,
                ret: Some(f(Sc::F64)),
                extra,
                system: false,
            });
        }
        out
    }

    /// The leaves of a value of `t` at `path`, with random bits.
    fn leaves(&mut self, t: &Ty, path: &str, out: &mut Vec<Leaf>) {
        match t {
            Ty::Sc(s) => out.push(Leaf {
                path: path.to_owned(),
                lane: None,
                sc: *s,
                bits: s.bits(&mut self.rng),
            }),
            Ty::Vec(e, n) => {
                for k in 0..*n {
                    out.push(Leaf {
                        path: path.to_owned(),
                        lane: Some(k),
                        sc: *e,
                        bits: e.bits(&mut self.rng),
                    });
                }
            }
            Ty::Agg(i) => {
                let a = self.aggs[*i].clone();
                // A union: only its first member, the largest.
                let n = if a.union { 1 } else { a.fields.len() };
                for (j, (f, len)) in a.fields.iter().take(n).enumerate() {
                    if *len == 0 {
                        self.leaves(f, &format!("{path}.f{j}"), out);
                    } else {
                        for k in 0..*len {
                            self.leaves(f, &format!("{path}.f{j}[{k}]"), out);
                        }
                    }
                }
            }
        }
    }
}

/// A value: its leaves.
struct Val(Vec<Leaf>);

impl Val {
    fn sum(&self, h: u64) -> u64 {
        self.0.iter().fold(h, |h, l| mix(h, l.bits))
    }
}

/// Type names in each language.
struct Names<'a> {
    g: &'a Gen,
}

impl Names<'_> {
    fn nabi(&self, t: &Ty) -> String {
        match t {
            Ty::Sc(s) => s.nabi(),
            Ty::Vec(e, n) => format!("Simd<{}, {n}>", e.nabi()),
            Ty::Agg(i) => format!("{}{i}", if self.g.aggs[*i].union { "U" } else { "S" }),
        }
    }

    fn c(&self, t: &Ty) -> String {
        match t {
            Ty::Sc(s) => s.c().to_owned(),
            Ty::Vec(e, n) => format!("v{n}_{}", e.c().replace(' ', "_")),
            Ty::Agg(i) => format!("{}{i}", if self.g.aggs[*i].union { "U" } else { "S" }),
        }
    }
}

/// A nabi expression of value `v` of type `t` (leaves in `leaves`
/// order).
fn nabi_expr(g: &Gen, t: &Ty, leaves: &mut std::slice::Iter<'_, Leaf>) -> String {
    match t {
        Ty::Sc(s) => s.nabi_lit(leaves.next().unwrap().bits),
        Ty::Vec(e, n) => {
            let lanes: Vec<String> = (0..*n)
                .map(|_| e.nabi_lit(leaves.next().unwrap().bits))
                .collect();
            format!("Simd::from_array([{}])", lanes.join(", "))
        }
        Ty::Agg(i) => {
            let a = &g.aggs[*i];
            let name = Names { g }.nabi(t);
            let n = if a.union { 1 } else { a.fields.len() };
            let fields: Vec<String> = a
                .fields
                .iter()
                .take(n)
                .enumerate()
                .map(|(j, (f, len))| {
                    let e = if *len == 0 {
                        nabi_expr(g, f, leaves)
                    } else {
                        let xs: Vec<String> = (0..*len).map(|_| nabi_expr(g, f, leaves)).collect();
                        format!("[{}]", xs.join(", "))
                    };
                    format!("f{j}: {e}")
                })
                .collect();
            format!("{name} {{ {} }}", fields.join(", "))
        }
    }
}

fn nabi_leaf(var: &str, l: &Leaf) -> String {
    match l.lane {
        Some(k) => l.sc.nabi_bits(&format!("{var}{}.to_array()[{k}]", l.path)),
        None => l.sc.nabi_bits(&format!("{var}{}", l.path)),
    }
}

fn c_leaf(var: &str, l: &Leaf) -> String {
    match l.lane {
        Some(k) => l.sc.c_bits(&format!("{var}{}[{k}]", l.path)),
        None => l.sc.c_bits(&format!("{var}{}", l.path)),
    }
}

/// C statements setting `var` (declared already) to `v`.
fn c_set(var: &str, v: &Val, out: &mut String) {
    let _ = writeln!(out, "    zero(&{var}, sizeof {var});");
    for l in &v.0 {
        let lane = l.lane.map(|k| format!("[{k}]")).unwrap_or_default();
        let _ = writeln!(out, "    {var}{}{lane} = {};", l.path, l.sc.c_lit(l.bits));
    }
}

const C_HEAD: &str = r"
#include <stdarg.h>
#if defined(_MSC_VER) && defined(_M_IX86)
#define SYS __stdcall
#elif defined(_WIN32) && defined(__i386__)
#define SYS __attribute__((stdcall))
#else
#define SYS
#endif
typedef unsigned long long u64;
#if defined(_MSC_VER) && !defined(__clang__)
#include <string.h>
#define cp memcpy
#define zero(d, n) memset(d, 0, n)
#else
#define cp __builtin_memcpy
#define zero(d, n) __builtin_memset(d, 0, n)
#endif
static u64 mix(u64 h, u64 x) { return (h ^ x) * 0x100000001b3ull; }
static float fv(unsigned u) { float f; cp(&f, &u, 4); return f; }
static double dv(u64 u) { double d; cp(&d, &u, 8); return d; }
static u64 fb(float f) { unsigned u; cp(&u, &f, 4); return u; }
static u64 db(double d) { u64 u; cp(&u, &d, 8); return u; }
";

const C_HALF: &str = r"
static _Float16 hv(unsigned short u) { _Float16 h; cp(&h, &u, 2); return h; }
static u64 hb(_Float16 h) { unsigned short u; cp(&u, &h, 2); return u; }
";

const C_BF16: &str = r"
static __bf16 bv(unsigned short u) { __bf16 h; cp(&h, &u, 2); return h; }
static u64 bb(__bf16 h) { unsigned short u; cp(&u, &h, 2); return u; }
";

/// Both sides of the cases: `(C, nabi)`.
fn program(g: &mut Gen, cases: &[Case]) -> (String, String) {
    // Values first: they take the generator's random bits.
    let mut vals: Vec<(Vec<Val>, Option<Val>, Vec<Leaf>)> = Vec::new();
    for c in cases {
        let args = c
            .params
            .iter()
            .map(|t| {
                let mut ls = Vec::new();
                g.leaves(t, "", &mut ls);
                Val(ls)
            })
            .collect();
        let ret = c.ret.as_ref().map(|t| {
            let mut ls = Vec::new();
            g.leaves(t, "", &mut ls);
            Val(ls)
        });
        let extra = c
            .extra
            .iter()
            .map(|s| Leaf {
                path: String::new(),
                lane: None,
                sc: *s,
                bits: s.bits(&mut g.rng),
            })
            .collect();
        vals.push((args, ret, extra));
    }
    let g = &*g;
    let names = Names { g };
    let mut c = String::from(C_HEAD);
    if g.feats.f16 {
        c.push_str(C_HALF);
    }
    if g.feats.bf16 {
        c.push_str(C_BF16);
    }
    let mut n = String::from("use core::simd::Simd;\n\n");
    // Types.
    let mut vecs: Vec<(Sc, u32)> = Vec::new();
    for case in cases {
        for t in case.params.iter().chain(&case.ret) {
            if let Ty::Vec(e, k) = t {
                if !vecs.contains(&(*e, *k)) {
                    vecs.push((*e, *k));
                }
            }
        }
    }
    for (e, k) in &vecs {
        let size = e.size(g.feats.wide).0 * k;
        let t = names.c(&Ty::Vec(*e, *k));
        let _ = writeln!(
            c,
            "typedef {} {t} __attribute__((vector_size({size})));",
            e.c()
        );
    }
    for (i, a) in g.aggs.iter().enumerate() {
        let ty = Ty::Agg(i);
        let kw = if a.union { "union" } else { "struct" };
        let mut attrs = String::new();
        let msvc = g.feats.msvc;
        if a.packed {
            if msvc {
                c.push_str("#pragma pack(push, 1)\n");
            } else {
                attrs.push_str(" __attribute__((packed))");
            }
            n.push_str("#[repr(packed)]\n");
        }
        if a.align > 0 {
            if msvc {
                let _ = write!(attrs, " __declspec(align({}))", a.align);
            } else {
                let _ = write!(attrs, " __attribute__((aligned({})))", a.align);
            }
            let _ = writeln!(n, "#[repr(align({}))]", a.align);
        }
        let _ = write!(c, "typedef {kw}{attrs} {{");
        let _ = write!(n, "{kw} {} {{", names.nabi(&ty));
        for (j, (f, len)) in a.fields.iter().enumerate() {
            if *len == 0 {
                let _ = write!(c, " {} f{j};", names.c(f));
                let _ = write!(n, " f{j}: {};", names.nabi(f));
            } else {
                let _ = write!(c, " {} f{j}[{len}];", names.c(f));
                let _ = write!(n, " f{j}: [{}; {len}];", names.nabi(f));
            }
        }
        let _ = writeln!(c, " }} {};", names.c(&ty));
        if a.packed && msvc {
            c.push_str("#pragma pack(pop)\n");
        }
        let _ = writeln!(n, " }}");
    }
    let count = cases.len();
    let _ = writeln!(
        c,
        "u64 c_seen[{count}];\nu64 nabi_seen[{count}];\n\
         void c_note(int i, u64 h) {{ nabi_seen[i] = h; }}\n\
         u64 c_seen_at(int i) {{ return c_seen[i]; }}"
    );
    let mut decls = String::new();
    let mut system_decls = String::new();
    let mut defs = String::new();
    let mut checks = String::new();
    let mut drives = String::new();
    for (i, (case, (args, ret, extra))) in cases.iter().zip(&vals).enumerate() {
        let want = args.iter().fold(FNV, |h, v| v.sum(h));
        let want = extra.iter().fold(want, |h, l| mix(h, l.bits));
        let sys = if case.system { "SYS " } else { "" };
        let c_ret = case.ret.as_ref().map_or("void".to_owned(), |t| names.c(t));
        let n_ret = case
            .ret
            .as_ref()
            .map_or(String::new(), |t| format!(" -> {}", names.nabi(t)));
        let c_params: Vec<String> = case
            .params
            .iter()
            .enumerate()
            .map(|(j, t)| format!("{} a{j}", names.c(t)))
            .collect();
        let n_params: Vec<String> = case
            .params
            .iter()
            .enumerate()
            .map(|(j, t)| format!("a{j}: {}", names.nabi(t)))
            .collect();
        let variadic = !case.extra.is_empty();
        let c_tail = if variadic { ", ..." } else { "" };
        // C callee.
        let c_list = if c_params.is_empty() {
            "void".to_owned()
        } else {
            c_params.join(", ")
        };
        let _ = writeln!(c, "{c_ret} {sys}c_f{i}({c_list}{c_tail}) {{");
        let _ = writeln!(c, "    u64 h = 0x{FNV:x}ull;");
        for (j, v) in args.iter().enumerate() {
            for l in &v.0 {
                let _ = writeln!(c, "    h = mix(h, {});", c_leaf(&format!("a{j}"), l));
            }
        }
        if variadic {
            let last = case.params.len() - 1;
            let _ = writeln!(c, "    va_list ap;\n    va_start(ap, a{last});");
            for l in extra {
                let read = match l.sc {
                    Sc::F64 => "va_arg(ap, double)".to_owned(),
                    Sc::F32 => "(float)va_arg(ap, double)".to_owned(),
                    Sc::I(64, _) => "va_arg(ap, long long)".to_owned(),
                    s => format!("({})va_arg(ap, int)", s.c()),
                };
                let _ = writeln!(c, "    h = mix(h, {});", l.sc.c_bits(&read));
            }
            let _ = writeln!(c, "    va_end(ap);");
        }
        let _ = writeln!(c, "    c_seen[{i}] = h;");
        if let Some(r) = ret {
            let _ = writeln!(c, "    {c_ret} r;");
            c_set("r", r, &mut c);
            let _ = writeln!(c, "    return r;");
        }
        let _ = writeln!(c, "}}");
        // nabi's declaration of it.
        let decl = format!("    fn c_f{i}({}{c_tail}){n_ret};\n", n_params.join(", "));
        if case.system {
            system_decls.push_str(&decl);
        } else {
            decls.push_str(&decl);
        }
        // nabi callee and C caller, without a variadic tail.
        if !variadic {
            let abi = if case.system { "system" } else { "C" };
            let _ = writeln!(
                defs,
                "#[export_name(\"nabi_f{i}\")]\nextern \"{abi}\" fn n_f{i}({}){n_ret} {{",
                n_params.join(", ")
            );
            let _ = writeln!(defs, "    let h = {FNV}u64;");
            let _ = writeln!(defs, "    unsafe {{");
            for (j, v) in args.iter().enumerate() {
                for l in &v.0 {
                    let _ = writeln!(
                        defs,
                        "        h = mix(h, {});",
                        nabi_leaf(&format!("a{j}"), l)
                    );
                }
            }
            let _ = writeln!(defs, "        c_note({i}, h);\n    }}");
            if let (Some(t), Some(r)) = (&case.ret, ret) {
                let _ = writeln!(defs, "    {}", nabi_expr(g, t, &mut r.0.iter()));
            }
            let _ = writeln!(defs, "}}\n");
            let _ = writeln!(c, "{c_ret} {sys}nabi_f{i}({c_list});");
            let _ = writeln!(c, "static int c_drive{i}(void) {{\n    int bad = 0;");
            for (j, (t, v)) in case.params.iter().zip(args).enumerate() {
                let _ = writeln!(c, "    {} a{j};", names.c(t));
                c_set(&format!("a{j}"), v, &mut c);
            }
            let call = format!(
                "nabi_f{i}({})",
                (0..case.params.len())
                    .map(|j| format!("a{j}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            match ret {
                Some(r) => {
                    let _ = writeln!(c, "    {c_ret} r = {call};");
                    for l in &r.0 {
                        let _ = writeln!(
                            c,
                            "    if ({} != 0x{:x}ull) bad |= 2;",
                            c_leaf("r", l),
                            l.bits
                        );
                    }
                }
                None => {
                    let _ = writeln!(c, "    {call};");
                }
            }
            let _ = writeln!(
                c,
                "    if (nabi_seen[{i}] != 0x{want:x}ull) bad |= 1;\n    return bad;\n}}"
            );
            let _ = writeln!(drives, "    case {i}: return c_drive{i}();");
        }
        // nabi caller.
        let lits: Vec<String> = case
            .params
            .iter()
            .zip(args)
            .map(|(t, v)| nabi_expr(g, t, &mut v.0.iter()))
            .chain(extra.iter().map(|l| l.sc.nabi_lit(l.bits)))
            .collect();
        let _ = writeln!(checks, "fn case{i}() -> i32 {{\n    let bad = 0;");
        let call = |callee: &str, ret_bit: u32, seen_bit: u32, out: &mut String| {
            let _ = writeln!(out, "    {{");
            if ret.is_some() {
                let _ = writeln!(
                    out,
                    "        let r = unsafe {{ {callee}({}) }};",
                    lits.join(", ")
                );
            } else {
                let _ = writeln!(out, "        unsafe {{ {callee}({}) }};", lits.join(", "));
            }
            if let Some(r) = ret {
                for l in &r.0 {
                    let _ = writeln!(
                        out,
                        "        if unsafe {{ {} }} != {}u64 {{ bad = bad | {ret_bit}; }}",
                        nabi_leaf("r", l),
                        l.bits
                    );
                }
            }
            let _ = writeln!(
                out,
                "        if unsafe {{ c_seen_at({i}) }} != {want}u64 {{ bad = bad | {seen_bit}; }}\n    }}"
            );
        };
        call(&format!("c_f{i}"), 2, 1, &mut checks);
        if i % 4 == 0 && !variadic {
            // Through an `extern "C" fn` value.
            let abi = if case.system { "system" } else { "C" };
            let ptys: Vec<String> = case.params.iter().map(|t| names.nabi(t)).collect();
            let _ = writeln!(
                checks,
                "    let f: unsafe extern \"{abi}\" fn({}){n_ret} = c_f{i};",
                ptys.join(", ")
            );
            call("f", 8, 4, &mut checks);
        }
        if !variadic {
            let _ = writeln!(checks, "    bad = bad | unsafe {{ c_drive({i}) }} * 16;");
        }
        let _ = writeln!(checks, "    bad\n}}\n");
    }
    let _ = writeln!(
        c,
        "int c_drive(int i) {{\n    switch (i) {{\n{drives}    }}\n    return 64;\n}}"
    );
    let _ = writeln!(
        n,
        "\nextern \"C\" {{\n{decls}    fn c_seen_at(i: i32) -> u64;\n    fn c_note(i: i32, h: u64);\n    fn c_drive(i: i32) -> i32;\n}}\n"
    );
    if !system_decls.is_empty() {
        let _ = writeln!(n, "extern \"system\" {{\n{system_decls}}}\n");
    }
    let _ = writeln!(
        n,
        "fn mix(h: u64, x: u64) -> u64 {{ (h ^ x).wrapping_mul({PRIME}u64) }}\n"
    );
    n.push_str(&defs);
    n.push_str(&checks);
    n.push_str("fn main() -> i32 {\n    let failed = 0;\n");
    for i in 0..count {
        let _ = writeln!(
            n,
            "    let b{i} = case{i}();\n    if b{i} != 0 {{ println(f\"case {i}: {{b{i}}}\"); failed += 1; }}"
        );
    }
    let _ = writeln!(
        n,
        "    println(f\"{count} cases, {{failed}} failed\");\n    0\n}}"
    );
    (c, n)
}

fn feats() -> Feats {
    Feats::of(&crate::common::spec())
}

/// What the target's C passes and the compiler `cc` takes: MSVC's C has
/// no `_Float16`, `__bf16` or GNU vectors, and an older gcc may lack one.
fn feats_of(cc: &crate::native::ffi::Cc, dir: &Path) -> Feats {
    let mut f = feats();
    f.msvc = cc.msvc;
    let has = |code: &str| !cc.msvc && cc.accepts(dir, code);
    f.f16 = f.f16 && has("_Float16 h(_Float16 x) { return x; }\n");
    f.half_ret &= f.f16;
    f.bf16 = f.bf16 && has("__bf16 b(__bf16 x) { return x; }\n");
    f.bf16_agg = !(cc.gcc() && matches!(crate::common::spec().arch.as_str(), "aarch64" | "arm"));
    f.odd_small_ret = !(cc.cl && !cc.msvc && crate::common::spec().arch == "x86");
    f.packed_stack_agg = !(cc.gcc() && crate::common::spec().vendor == "apple");
    f.simd = f.simd
        && has("typedef float v __attribute__((vector_size(16)));\nv f(v a) { return a; }\n");
    f
}

/// The table and the random cases against the C compiler (`Cc`). A failing case prints
/// `case <n>: <bits>`: 1 C saw other arguments, 2 nabi got another
/// result, 4 / 8 the same through an `extern "C" fn` value, 16 / 32 C
/// calling nabi (arguments / result).
#[test]
fn c_abi_table() {
    let env = setup();
    let seed = std::env::var("NABI_ABI_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SEED);
    let count = std::env::var("NABI_ABI_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(160);
    let dir = scratch("cabitable");
    let cc = crate::native::ffi::Cc::get();
    let f = feats_of(&cc, &dir);
    // Which compiler, and what it could not check.
    eprintln!(
        "C ABI table against `{}`: f16 {}, bf16 {}, simd {}",
        cc.name(),
        f.f16,
        f.bf16,
        f.simd
    );
    let mut g = Gen::new(f, seed);
    let mut cases = g.table();
    let fixed = cases.len();
    for _ in 0..count {
        let c = g.rand_case();
        cases.push(c);
    }
    let (c, n) = program(&mut g, &cases);
    fs::write(dir.join("side.c"), c).unwrap();
    fs::write(dir.join("main.nabi"), n).unwrap();
    let run = c_program(&env, &dir, &[]);
    ok(&run);
    let out = text(&run.stdout);
    assert!(
        out.ends_with(&format!("{} cases, 0 failed\n", cases.len())),
        "`{}`, seed {seed}, {fixed} fixed cases first; in {}:\n{out}",
        cc.name(),
        dir.display()
    );
}

/// Where the target's C has no `_Float16` / `__bf16` or no vectors, a
/// signature with one is an error that says what to write.
#[test]
fn unsupported_types_are_refused() {
    let f = feats();
    let env = setup();
    let mut probes = Vec::new();
    if !f.f16 {
        probes.push(("fn h(x: f16) -> i32;", "takes no `_Float16` argument"));
    }
    if !f.bf16 {
        probes.push(("fn b() -> bf16;", "takes no `__bf16` result"));
    }
    if !f.half_ret && f.f16 {
        probes.push(("fn h() -> f16;", "takes no `_Float16` result"));
    }
    if !f.simd {
        probes.push(("fn v(x: Simd<f32, 4>);", "cannot cross the C ABI"));
    }
    let probes: Vec<(String, &str)> = probes
        .into_iter()
        .map(|(d, w)| {
            let name = d.trim_start_matches("fn ").split('(').next().unwrap();
            (
                format!("extern \"C\" {{ {d} }}\nfn main() {{ let f = {name}; }}"),
                w,
            )
        })
        .chain([
            (
                "extern \"C\" { fn v(n: i32, ...); }\n\
                 fn main() { unsafe { v(1, f16::from_bits(0u16)) }; }"
                    .to_owned(),
                "variadic",
            ),
            (
                "extern \"C\" { fn s(x: Simd<f32, 3>); }\nfn main() { let f = s; }".to_owned(),
                "cannot cross the C ABI",
            ),
        ])
        .collect();
    for (i, (prog, want)) in probes.iter().enumerate() {
        let dir = scratch(&format!("cabirefuse{i}"));
        let src = format!("use core::simd::Simd;\n{prog}\n");
        fs::write(dir.join("main.nabi"), src).unwrap();
        let out = build(
            &env,
            &dir.join("main.nabi"),
            &["--emit=obj", "-o", dir.join("m.o").to_str().unwrap()],
        );
        let err = text(&out.stderr);
        assert!(!out.status.success() && err.contains(want), "{prog}: {err}");
        assert!(err.contains("help:"), "{prog}: {err}");
    }
}
