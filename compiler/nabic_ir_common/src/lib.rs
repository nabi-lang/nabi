//! Shared IR primitives used by HIR and MIR.

pub mod typedesc;

use nabic_core::DefId;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockId(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalId(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FunctionId(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PrimTy {
    I8,
    I16,
    I32,
    I64,
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
    F32,
    F64,
    Bool,
    /// C `char` (FFI; `c'A'`).
    CChar,
    /// Unicode scalar value; 32-bit code point.
    Char,
    /// stdlib `str` (UTF-8 view, primitive).
    Str,
}

impl PrimTy {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::Isize => "isize",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::Usize => "usize",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::CChar => "c_char",
            Self::Char => "char",
            Self::Str => "str",
        }
    }

    #[must_use]
    pub const fn is_signed_int(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::Isize
        )
    }

    #[must_use]
    pub const fn is_unsigned_int(self) -> bool {
        matches!(
            self,
            Self::U8 | Self::U16 | Self::U32 | Self::U64 | Self::Usize
        )
    }

    #[must_use]
    pub const fn is_int(self) -> bool {
        self.is_signed_int() || self.is_unsigned_int() || matches!(self, Self::CChar | Self::Char)
    }

    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    /// Bit width given the target pointer width (32 or 64).
    /// Returns 0 for `Str` since it lowers to (ptr, len).
    #[must_use]
    pub const fn bit_width(self, pointer_width: u32) -> u32 {
        match self {
            Self::I8 | Self::U8 | Self::Bool | Self::CChar => 8,
            Self::I16 | Self::U16 => 16,
            Self::I32 | Self::U32 | Self::F32 | Self::Char => 32,
            Self::I64 | Self::U64 | Self::F64 => 64,
            Self::Isize | Self::Usize => pointer_width,
            Self::Str => 0,
        }
    }
}

/// Primitive op after stdlib-trait desugaring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    /// Floored `%%` (`nabi.mod.TYPE`): the result takes the divisor's sign.
    /// MIR expands it, so backends never see it.
    Mod,
    Neg,
    Not,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LogicalAnd,
    LogicalOr,
    LogicalNot,
    /// Square root (floats; `nabi.sqrt.TYPE`, `√`).
    Sqrt,
    /// Integer `+ - *` that wrap even where overflow checks are on
    /// (`nabi.wrapping_add.TYPE` ..; the `wrapping_*` methods).
    WrappingAdd,
    WrappingSub,
    WrappingMul,
    /// Bit counts / byte swap on integers, same type in and out
    /// (`nabi.ctpop` / `nabi.ctlz` / `nabi.cttz` / `nabi.bswap`).
    CountOnes,
    LeadingZeros,
    TrailingZeros,
    SwapBytes,
    /// The bits of an unsigned integer rotated left by the second
    /// operand (same type) modulo the width. Only MIR makes it, of the
    /// shifts that spell one.
    RotateLeft,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstVal {
    Unit,
    Bool(bool),
    Int {
        value: i128,
        ty: PrimTy,
    },
    UInt {
        value: u128,
        ty: PrimTy,
    },
    Float {
        bits: u64,
        ty: PrimTy,
    },
    Char(u32),
    Str(String),
    /// A `[u8; N]` (`b".."`, `include_bytes!`). MIR lowering expands it
    /// to its elements; it reaches codegen only as a static's value.
    Bytes(Vec<u8>),
    None,
}

/// Calling convention of a function.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Abi {
    /// nabi's own: `str` is `(ptr, len)` by value, a `fn` value
    /// `{code, env}`. Every nabi function and `extern "nabi"` declaration.
    #[default]
    Nabi,
    /// Platform C ABI: `str` is a NUL-terminated `char*`, `fn`
    /// a bare function pointer, aggregates classified. `system`:
    /// `stdcall` on 32-bit Windows, C elsewhere.
    C { variadic: bool, system: bool },
}

/// Where a function's code comes from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Linkage {
    /// Its body is compiled here.
    #[default]
    Defined,
    /// A declaration of an outside symbol, under its own name.
    Extern,
    /// A declaration of a prebuilt dependency's function:
    /// mangled, in the dependency's archive.
    Prebuilt,
}

/// `#[inline]` / `#[inline(always)]` / `#[inline(never)]`,
/// or `#[cold]`: never inlined, laid out away from hot code.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Inline {
    #[default]
    Auto,
    Hint,
    Always,
    Never,
    Cold,
}

/// A function the back end treats specially.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FnRole {
    #[default]
    Plain,
    /// The user's `main`: the C `main` wraps it.
    Main,
    /// An `any Tr` method dispatcher: weak in a prebuilt library, so the
    /// program's complete one replaces it.
    AnyDispatcher,
    /// A C function (`nabi cc`): its body takes and returns its values
    /// the C way under its C name, keeps no runtime state (no thread
    /// block, GC frame, polls or checks), and its C calls do not park.
    C,
}

/// Per-function facts HIR decides and MIR / codegen only read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // independent facts, not a state
pub struct FnFacts {
    pub abi: Abi,
    pub linkage: Linkage,
    /// `#[export_name("sym")]`.
    pub export_name: Option<String>,
    /// wasm32 core import `(module, name)`: `#[link(wasm_import_module)]`
    /// and `#[link_name]` or the own name.
    pub wasm_import: Option<(String, String)>,
    /// `#[link_name = "sym"]` of an extern function outside a
    /// `wasm_import_module` block: the symbol it links to.
    pub link_name: Option<String>,
    pub role: FnRole,
    /// A generic instance's mangled type arguments; the name ends
    /// in `MONO_MARK` + these.
    pub mono_args: Option<String>,
    /// `#[track_caller]`: the last parameter is the caller's location
    /// (`str`, `path:line:col`), which its panics report.
    pub track_caller: bool,
    /// Declared in a `#[no_safepoint] extern "nabi"` block: neither
    /// allocates nor blocks, so a call to it is no safepoint.
    pub no_safepoint: bool,
    /// `#[errno]` on a C declaration: errno (and `GetLastError` on
    /// Windows) is saved right after the call returns.
    pub errno: bool,
    /// `#[leaf]` on a C declaration: the call neither blocks nor calls
    /// back, so the thread does not park for it.
    pub leaf: bool,
    /// `#[on_panic(return = v)]` on an `extern "C" fn` definition: a
    /// panic in it returns `v` to C instead of aborting.
    pub on_panic: Option<ConstVal>,
    pub inline: Inline,
    /// What a call of it does, known without its body (`Summary`).
    pub summary: Summary,
    /// A `core::simd` lane operation (`#[intrinsic("nabi.simd.OP")]`):
    /// a back end may emit a call as vector code; the body is the lane
    /// loop it stands for.
    pub simd: Option<SimdOp>,
}

/// What a `core::simd` operation does to each lane of its vector
/// arguments (one-field structs over `[T; N]`) and result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdOp {
    /// `Add` `Sub` `Mul` (wrapping for integers), `Div`, `BitAnd`
    /// `BitOr` `BitXor`; `Eq` `Ne` `Lt` `Le` give a mask lane (all bits
    /// set when true).
    Bin(PrimOp),
    /// `Neg` (wrapping), `Not`.
    Un(PrimOp),
    /// The lesser / greater lane of each pair (`b < a`, `b > a`).
    Min,
    Max,
    /// The magnitude (wrapping at the least signed integer).
    Abs,
    /// `mask, a, b`: the lane of `a` where the mask's is set, else `b`'s.
    Select,
    /// `buf, at`: the `N` elements of a `GcBuf` from `at` (in bounds).
    Load,
    /// `v, buf, at`: the lanes stored to the elements from `at`.
    Store,
    /// `x`: every lane `x`.
    Splat,
    /// `v`: zero plus each lane, first to last (integers wrap).
    Sum,
}

impl SimdOp {
    /// The op `#[intrinsic("nabi.simd.NAME")]` names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let op = name.strip_prefix("nabi.simd.")?;
        Some(match op {
            "add" => Self::Bin(PrimOp::Add),
            "sub" => Self::Bin(PrimOp::Sub),
            "mul" => Self::Bin(PrimOp::Mul),
            "div" => Self::Bin(PrimOp::Div),
            "and" => Self::Bin(PrimOp::BitAnd),
            "or" => Self::Bin(PrimOp::BitOr),
            "xor" => Self::Bin(PrimOp::BitXor),
            "min" => Self::Min,
            "max" => Self::Max,
            "eq" => Self::Bin(PrimOp::Eq),
            "ne" => Self::Bin(PrimOp::Ne),
            "lt" => Self::Bin(PrimOp::Lt),
            "le" => Self::Bin(PrimOp::Le),
            "neg" => Self::Un(PrimOp::Neg),
            "not" => Self::Un(PrimOp::Not),
            "abs" => Self::Abs,
            "select" => Self::Select,
            "load" => Self::Load,
            "store" => Self::Store,
            "splat" => Self::Splat,
            "sum" => Self::Sum,
            _ => return None,
        })
    }
}

/// What a call does, as the callee's package showed when it was built.
///
/// A prebuilt library's function (its NABIHIR) or a runtime function
/// (`nabic_mir::summary::runtime`). Each fact holds of the code that
/// runs, so a call may rely on it with the body elsewhere. A fact not
/// set is unknown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Summary {
    /// `READONLY`, `QUIET`, `COLD`.
    pub flags: u32,
    /// Bit `i`: parameter `i`'s value lives on only in the result, if
    /// in `returns`: it is not stored, captured or passed where it may
    /// be.
    pub keeps_none: u64,
    /// Bit `i`: the result may be parameter `i`'s value.
    pub returns: u64,
    /// A field of the result that is a parameter handed back (`ref`).
    pub echo: Echo,
}

/// A parameter a tuple result hands back, and its fields that may change.
///
/// Field `field - 1` of the result (0: none) is parameter `param` as the
/// caller passed it, but for its fields in `changed` (bit `i`: field `i`;
/// bit 63: any field from 63 on). Known from the body only, never
/// exported: a caller writes back just those fields.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Echo {
    pub field: u32,
    pub param: u32,
    pub changed: u64,
}

impl Summary {
    /// Writes no memory its caller reads. It may still allocate, reach
    /// a safepoint and panic.
    pub const READONLY: u32 = 1;
    /// Never reaches a safepoint: no allocation, poll or blocking.
    pub const QUIET: u32 = 2;
    /// Rarely called: `#[cold]`, or never returns.
    pub const COLD: u32 = 4;

    #[must_use]
    pub const fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.flags == 0 && self.keeps_none == 0
    }

    /// Whether parameter `i` is not kept past the call (`keeps_none`).
    #[must_use]
    pub const fn keeps_none(&self, i: usize) -> bool {
        i < 64 && (self.keeps_none >> i) & 1 != 0
    }
}

/// Separator between a generic function's path and its instance's type
/// arguments in the instance's name.
pub const MONO_MARK: &str = "__mono__";

impl FnFacts {
    /// The item path of a function named `name` (an instance's generic).
    #[must_use]
    pub fn path<'a>(&self, name: &'a str) -> &'a str {
        self.mono_args
            .as_ref()
            .and_then(|a| name.strip_suffix(a.as_str()))
            .and_then(|n| n.strip_suffix(MONO_MARK))
            .unwrap_or(name)
    }

    /// Whether the body takes the caller's `nabi_tls` address as a hidden
    /// last argument. All but the `any` dispatcher and
    /// `#[export_name]` functions, which are entered from outside.
    #[must_use]
    pub fn takes_tls(&self) -> bool {
        self.linkage != Linkage::Extern
            && !matches!(self.role, FnRole::AnyDispatcher | FnRole::C)
            && !(self.abi == Abi::Nabi && self.export_name.is_some())
    }
}

/// Callee form after name resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedFn {
    /// Statically resolved.
    Free(DefId),
    /// any-slot or sum-type narrowing path; resolved at runtime.
    Dynamic(String),
}

macro_rules! codec_id {
    ($($t:ident),*) => {$(
        impl nabic_core::codec::Codec for $t {
            fn enc(&self, e: &mut nabic_core::codec::Enc) {
                e.u(u64::from(self.0));
            }
            fn dec(d: &mut nabic_core::codec::Dec<'_>) -> Option<Self> {
                Some(Self(u32::try_from(d.u()?).ok()?))
            }
        }
    )*};
}
codec_id!(BlockId, LocalId);

impl nabic_core::codec::Codec for FunctionId {
    fn enc(&self, e: &mut nabic_core::codec::Enc) {
        e.id(nabic_core::codec::IdKind::Function, self.0);
    }
    fn dec(d: &mut nabic_core::codec::Dec<'_>) -> Option<Self> {
        d.id(nabic_core::codec::IdKind::Function).map(Self)
    }
}

nabic_core::codec_enum!(PrimTy {
    I8,
    I16,
    I32,
    I64,
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
    F32,
    F64,
    Bool,
    CChar,
    Char,
    Str,
});
nabic_core::codec_enum!(PrimOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Neg,
    Not,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LogicalAnd,
    LogicalOr,
    LogicalNot,
    Sqrt,
    WrappingAdd,
    WrappingSub,
    WrappingMul,
    CountOnes,
    LeadingZeros,
    TrailingZeros,
    SwapBytes,
    Mod,
    RotateLeft,
});
nabic_core::codec_enum!(ConstVal {
    Unit,
    Bool(x0),
    Int { value, ty },
    UInt { value, ty },
    Float { bits, ty },
    Char(x0),
    Str(x0),
    Bytes(x0),
    None,
});
nabic_core::codec_enum!(Abi {
    Nabi,
    C { variadic, system }
});
nabic_core::codec_enum!(Linkage {
    Defined,
    Extern,
    Prebuilt
});
nabic_core::codec_enum!(FnRole {
    Plain,
    Main,
    AnyDispatcher,
    C
});
nabic_core::codec_enum!(Inline {
    Auto,
    Hint,
    Always,
    Never,
    Cold
});
nabic_core::codec_struct!(FnFacts {
    abi,
    linkage,
    export_name,
    wasm_import,
    link_name,
    role,
    mono_args,
    track_caller,
    no_safepoint,
    errno,
    leaf,
    on_panic,
    inline,
    summary,
    simd,
});
nabic_core::codec_enum!(SimdOp {
    Bin(x0),
    Un(x0),
    Min,
    Max,
    Abs,
    Select,
    Load,
    Store,
    Splat,
    Sum
});
nabic_core::codec_struct!(Summary {
    flags,
    keeps_none,
    returns,
    echo
});
nabic_core::codec_struct!(Echo {
    field,
    param,
    changed
});
