//! The C ABI of a target as clang has it: type sizes and alignments,
//! record layout rules and the predefined macros system headers test.

// Expression and layout code names values as C does (`a`, `b`, `l`, `r`).
#![allow(clippy::many_single_char_names)]

use std::fmt::Write as _;

/// A builtin arithmetic type.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Prim {
    Void,
    Bool,
    Char,
    SChar,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    LongLong,
    ULongLong,
    Int128,
    UInt128,
    Float16,
    Fp16,
    BF16,
    Float,
    Double,
    LongDouble,
    Float128,
    Ibm128,
}

impl Prim {
    pub const fn is_integer(self) -> bool {
        matches!(
            self,
            Self::Bool
                | Self::Char
                | Self::SChar
                | Self::UChar
                | Self::Short
                | Self::UShort
                | Self::Int
                | Self::UInt
                | Self::Long
                | Self::ULong
                | Self::LongLong
                | Self::ULongLong
                | Self::Int128
                | Self::UInt128
        )
    }

    pub const fn is_float(self) -> bool {
        matches!(
            self,
            Self::Float16
                | Self::Fp16
                | Self::BF16
                | Self::Float
                | Self::Double
                | Self::LongDouble
                | Self::Float128
                | Self::Ibm128
        )
    }

    /// Integer conversion rank.
    pub const fn rank(self) -> u32 {
        match self {
            Self::Char | Self::SChar | Self::UChar => 1,
            Self::Short | Self::UShort => 2,
            Self::Int | Self::UInt => 3,
            Self::Long | Self::ULong => 4,
            Self::LongLong | Self::ULongLong => 5,
            Self::Int128 | Self::UInt128 => 6,
            _ => 0,
        }
    }

    #[must_use]
    pub const fn to_unsigned(self) -> Self {
        match self {
            Self::Char | Self::SChar => Self::UChar,
            Self::Short => Self::UShort,
            Self::Int => Self::UInt,
            Self::Long => Self::ULong,
            Self::LongLong => Self::ULongLong,
            Self::Int128 => Self::UInt128,
            p => p,
        }
    }

    /// The type's name as clang spells it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Bool => "_Bool",
            Self::Char => "char",
            Self::SChar => "signed char",
            Self::UChar => "unsigned char",
            Self::Short => "short",
            Self::UShort => "unsigned short",
            Self::Int => "int",
            Self::UInt => "unsigned int",
            Self::Long => "long",
            Self::ULong => "unsigned long",
            Self::LongLong => "long long",
            Self::ULongLong => "unsigned long long",
            Self::Int128 => "__int128",
            Self::UInt128 => "unsigned __int128",
            Self::Float16 => "_Float16",
            Self::Fp16 => "__fp16",
            Self::BF16 => "__bf16",
            Self::Float => "float",
            Self::Double => "double",
            Self::LongDouble => "long double",
            Self::Float128 => "__float128",
            Self::Ibm128 => "__ibm128",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    X86_64,
    X86,
    Aarch64,
    Arm,
    Riscv64,
    Riscv32,
    Ppc64le,
    S390x,
    Wasm32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Linux,
    Darwin,
    Windows,
    FreeBsd,
    Wasi,
    /// No operating system (bare metal, a browser).
    None,
}

/// The form of `long double`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LdFormat {
    Double,
    X87,
    Quad,
    IbmDouble,
}

/// What `__builtin_va_list` is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VaList {
    CharPtr,
    VoidPtr,
    /// x86-64 System V: `struct __va_list_tag[1]`.
    X86_64,
    /// `AArch64` AAPCS: `struct __va_list`.
    Aarch64,
    /// 32-bit ARM AAPCS: `struct __va_list { void *__ap; }`.
    Arm,
    /// s390x: `struct __va_list_tag[1]`.
    S390x,
}

/// The target facts bindgen passes in.
#[derive(Clone, Copy)]
pub struct TargetInfo<'a> {
    /// clang's triple, with any OS version.
    pub triple: &'a str,
    /// nabi's `target_arch`.
    pub arch: &'a str,
    pub os: &'a str,
    pub env: &'a str,
    pub vendor: &'a str,
    pub pointer_width: u32,
    pub big_endian: bool,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Target {
    pub triple: String,
    pub arch: Arch,
    pub os: Os,
    /// The MSVC environment: Microsoft record layout, `_MSC_VER`, no GNU.
    pub msvc: bool,
    pub musl: bool,
    /// Android's API level (bionic, Linux), else 0.
    pub android: u32,
    pub ptr: u64,
    pub long: u64,
    pub char_signed: bool,
    pub big_endian: bool,
    ld: LdFormat,
    ld_size: u64,
    ld_align: u64,
    /// Alignment of `long long` and `double` (4 on i386 System V).
    wide_align: u64,
    pub max_align: u64,
    /// `__attribute__((aligned))` without an argument.
    pub attr_align: u64,
    pub int128: bool,
    pub wchar: Prim,
    wint: Prim,
    pub size_t: Prim,
    pub ptrdiff_t: Prim,
    intptr: Prim,
    intmax: Prim,
    pub int64: Prim,
    pub va_list: VaList,
    /// Unnamed bitfields count toward a record's alignment (ARM AAPCS).
    pub unnamed_bitfield_align: bool,
    /// Symbols get a leading `_`.
    pub underscore: bool,
    /// Apple deployment version as `110000`.
    darwin_version: u32,
    /// iOS (else macOS), and its simulator.
    pub ios: bool,
    pub sim: bool,
    /// Objective-C headers (`-x objective-c`): `__OBJC__`, ARC's
    /// features and ownership qualifiers.
    pub objc: bool,
}

impl Target {
    pub fn new(t: &TargetInfo<'_>) -> Result<Self, String> {
        let arch = match t.arch {
            "x86_64" => Arch::X86_64,
            "x86" => Arch::X86,
            "aarch64" => Arch::Aarch64,
            "arm" => Arch::Arm,
            "riscv64" => Arch::Riscv64,
            "riscv32" => Arch::Riscv32,
            "powerpc64" if !t.big_endian => Arch::Ppc64le,
            "s390x" => Arch::S390x,
            "wasm32" => Arch::Wasm32,
            a => {
                return Err(format!(
                    "bindgen does not know the C ABI of architecture `{a}`{}; it knows x86_64, \
                     x86, aarch64, arm, riscv64, riscv32, powerpc64 (little-endian), s390x and \
                     wasm32",
                    if t.big_endian { " (big-endian)" } else { "" }
                ));
            }
        };
        let os = match t.os {
            "linux" | "android" => Os::Linux,
            "macos" | "ios" | "darwin" => Os::Darwin,
            "windows" => Os::Windows,
            "freebsd" => Os::FreeBsd,
            "wasi" => Os::Wasi,
            _ => Os::None,
        };
        // The API level ends clang's triple: `aarch64-linux-android29`.
        let android = if t.os == "android" {
            let last = t.triple.rsplit('-').next().unwrap_or("");
            let digits: String = last.chars().skip_while(|c| !c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(29)
        } else {
            0
        };
        let msvc = os == Os::Windows;
        if msvc && t.env != "msvc" {
            return Err(format!(
                "bindgen knows Windows only with the MSVC environment, not `{}`",
                t.env
            ));
        }
        let p64 = t.pointer_width == 64;
        let ptr = u64::from(t.pointer_width / 8);
        let long = if p64 && !msvc { 8 } else { 4 };
        let char_signed = match arch {
            Arch::X86_64 | Arch::X86 | Arch::Wasm32 => true,
            Arch::Aarch64 => os == Os::Darwin || msvc,
            _ => false,
        };
        let (ld, ld_size, ld_align) = match (arch, os) {
            (_, Os::Windows | Os::Darwin) | (Arch::Arm, _) => (LdFormat::Double, 8, 8),
            // Android: IEEE quad on x86_64 too.
            (Arch::X86_64, _) if android != 0 => (LdFormat::Quad, 16, 16),
            (Arch::X86_64, _) => (LdFormat::X87, 16, 16),
            (Arch::X86, _) => (LdFormat::X87, 12, 4),
            (Arch::Ppc64le, _) => (LdFormat::IbmDouble, 16, 16),
            (Arch::S390x, _) => (LdFormat::Quad, 16, 8),
            _ => (LdFormat::Quad, 16, 16),
        };
        let wide_align = if arch == Arch::X86 && !msvc { 4 } else { 8 };
        let max_align = match (arch, os) {
            (Arch::Aarch64, Os::Darwin) | (Arch::Arm | Arch::S390x, _) => 8,
            _ => 16,
        };
        let wchar = match (arch, os) {
            (_, Os::Windows) => Prim::UShort,
            (Arch::Aarch64 | Arch::Arm, Os::Linux | Os::FreeBsd | Os::None) => Prim::UInt,
            _ => Prim::Int,
        };
        let wint = match os {
            Os::Windows => Prim::UShort,
            Os::Darwin | Os::FreeBsd => Prim::Int,
            _ if arch == Arch::Wasm32 => Prim::Int,
            _ => Prim::UInt,
        };
        let int64 = if long == 8 && os != Os::Darwin {
            Prim::Long
        } else {
            Prim::LongLong
        };
        let (size_t, ptrdiff_t) = match (p64, msvc, arch) {
            (true, true, _) => (Prim::ULongLong, Prim::LongLong),
            (true, false, _) | (false, _, Arch::Wasm32) => (Prim::ULong, Prim::Long),
            (false, _, _) => (Prim::UInt, Prim::Int),
        };
        let intmax = if long == 8 {
            Prim::Long
        } else {
            Prim::LongLong
        };
        let va_list = match (arch, os) {
            (Arch::X86_64, Os::Windows) | (Arch::Aarch64, Os::Darwin | Os::Windows) => {
                VaList::CharPtr
            }
            (Arch::X86_64, _) => VaList::X86_64,
            (Arch::Aarch64, _) => VaList::Aarch64,
            (Arch::Arm, _) => VaList::Arm,
            (Arch::S390x, _) => VaList::S390x,
            (Arch::Riscv64 | Arch::Riscv32 | Arch::Wasm32, _) => VaList::VoidPtr,
            _ => VaList::CharPtr,
        };
        let darwin_version = if os == Os::Darwin {
            let v = t.triple.split('-').nth(2).unwrap_or("");
            let v = v.trim_start_matches(|c: char| c.is_ascii_alphabetic());
            let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
            let major = parts.next().unwrap_or(11).max(10);
            let minor = parts.next().unwrap_or(0);
            let patch = parts.next().unwrap_or(0);
            major * 10000 + minor * 100 + patch
        } else {
            0
        };
        Ok(Self {
            triple: t.triple.to_owned(),
            arch,
            os,
            msvc,
            musl: t.env == "musl",
            android,
            ptr,
            long,
            char_signed,
            big_endian: t.big_endian,
            ld,
            ld_size,
            ld_align,
            wide_align,
            max_align,
            attr_align: if matches!(arch, Arch::Arm | Arch::S390x) {
                8
            } else {
                16
            },
            int128: p64 || arch == Arch::Wasm32,
            wchar,
            wint,
            size_t,
            ptrdiff_t,
            intptr: ptrdiff_t,
            intmax,
            int64,
            va_list,
            unnamed_bitfield_align: matches!(arch, Arch::Arm | Arch::Aarch64)
                && os != Os::Darwin
                && !msvc,
            underscore: os == Os::Darwin || (msvc && arch == Arch::X86),
            darwin_version,
            ios: t.os == "ios",
            sim: t.os == "ios" && t.triple.ends_with("-simulator"),
            objc: false,
        })
    }

    /// The deployment version as `(major, minor, patch)`.
    pub const fn deployment(&self) -> (u32, u32, u32) {
        let v = self.darwin_version;
        (v / 10000, v / 100 % 100, v % 100)
    }

    /// Size and alignment in bytes.
    pub const fn prim(&self, p: Prim) -> (u64, u64) {
        match p {
            Prim::Void | Prim::Bool | Prim::Char | Prim::SChar | Prim::UChar => (1, 1),
            Prim::Short | Prim::UShort | Prim::Float16 | Prim::Fp16 | Prim::BF16 => (2, 2),
            Prim::Int | Prim::UInt | Prim::Float => (4, 4),
            Prim::Long | Prim::ULong => {
                if self.long == 8 {
                    (8, self.wide_align)
                } else {
                    (4, 4)
                }
            }
            Prim::LongLong | Prim::ULongLong | Prim::Double => (8, self.wide_align),
            Prim::Int128 | Prim::UInt128 => (
                16,
                if matches!(self.arch, Arch::S390x) {
                    8
                } else {
                    16
                },
            ),
            Prim::LongDouble => (self.ld_size, self.ld_align),
            Prim::Float128 | Prim::Ibm128 => (16, 16),
        }
    }

    pub const fn signed(&self, p: Prim) -> bool {
        match p {
            Prim::Char => self.char_signed,
            Prim::SChar | Prim::Short | Prim::Int | Prim::Long | Prim::LongLong | Prim::Int128 => {
                true
            }
            _ => false,
        }
    }

    pub const fn bits(&self, p: Prim) -> u32 {
        if matches!(p, Prim::Bool) {
            return 1;
        }
        (self.prim(p).0 * 8) as u32
    }

    /// clang's spelling of an integer type in the predefined macros.
    const fn int_name(p: Prim) -> &'static str {
        match p {
            Prim::SChar => "signed char",
            Prim::UChar => "unsigned char",
            Prim::Short => "short",
            Prim::UShort => "unsigned short",
            Prim::UInt => "unsigned int",
            Prim::Long => "long int",
            Prim::ULong => "long unsigned int",
            Prim::LongLong => "long long int",
            Prim::ULongLong => "long long unsigned int",
            _ => "int",
        }
    }

    const fn suffix(p: Prim) -> &'static str {
        match p {
            Prim::UInt => "U",
            Prim::Long => "L",
            Prim::ULong => "UL",
            Prim::LongLong => "LL",
            Prim::ULongLong => "ULL",
            _ => "",
        }
    }

    const fn fmt_mod(p: Prim) -> &'static str {
        match p {
            Prim::SChar | Prim::UChar | Prim::Char => "hh",
            Prim::Short | Prim::UShort => "h",
            Prim::Long | Prim::ULong => "l",
            Prim::LongLong | Prim::ULongLong => "ll",
            _ => "",
        }
    }

    fn max(&self, p: Prim) -> String {
        let bits = self.bits(p);
        let s = Self::suffix(p);
        if self.signed(p) {
            format!("{}{s}", (1u128 << (bits - 1)) - 1)
        } else {
            format!("{}{s}", (1u128 << bits) - 1)
        }
    }

    /// The `#define`s clang makes for this target, as source.
    #[allow(clippy::cognitive_complexity)]
    pub fn predefines(&self) -> String {
        let mut b = Defs(String::new());
        let gnu = !self.msvc;
        let ms = self.msvc;
        let darwin = self.os == Os::Darwin;
        b.d("__llvm__", "1");
        b.d("__clang__", "1");
        b.d("__clang_major__", "21");
        b.d("__clang_minor__", "0");
        b.d("__clang_patchlevel__", "0");
        b.d("__clang_version__", "\"21.0.0\"");
        b.d("__VERSION__", "\"Clang 21.0.0\"");
        b.d("__clang_literal_encoding__", "\"UTF-8\"");
        b.d(
            "__clang_wide_literal_encoding__",
            if ms { "\"UTF-16\"" } else { "\"UTF-32\"" },
        );
        if gnu {
            b.d("__GNUC__", "4");
            b.d("__GNUC_MINOR__", "2");
            b.d("__GNUC_PATCHLEVEL__", "1");
            b.d("__GNUC_STDC_INLINE__", "1");
            b.d("__GXX_ABI_VERSION", "1002");
            b.d("__STDC__", "1");
        }
        b.d("__STDC_HOSTED__", "1");
        b.d("__STDC_VERSION__", "201710L");
        b.d("__STDC_UTF_16__", "1");
        b.d("__STDC_UTF_32__", "1");
        b.d("__STDC_EMBED_NOT_FOUND__", "0");
        b.d("__STDC_EMBED_FOUND__", "1");
        b.d("__STDC_EMBED_EMPTY__", "2");
        if darwin || ms {
            b.d("__STDC_NO_THREADS__", "1");
        }
        if self.os == Os::FreeBsd {
            b.d("__STDC_MB_MIGHT_NEQ_WC__", "1");
        }
        b.d("__OBJC_BOOL_IS_BOOL", if darwin { "1" } else { "0" });
        b.d("__CONSTANT_CFSTRINGS__", "1");
        b.d("__NO_INLINE__", "1");
        b.d("__FINITE_MATH_ONLY__", "0");
        b.d("__PRAGMA_REDEFINE_EXTNAME", "1");
        b.d("__ORDER_LITTLE_ENDIAN__", "1234");
        b.d("__ORDER_BIG_ENDIAN__", "4321");
        b.d("__ORDER_PDP_ENDIAN__", "3412");
        if self.big_endian {
            b.d("__BYTE_ORDER__", "__ORDER_BIG_ENDIAN__");
            b.d("__BIG_ENDIAN__", "1");
        } else {
            b.d("__BYTE_ORDER__", "__ORDER_LITTLE_ENDIAN__");
            b.d("__LITTLE_ENDIAN__", "1");
        }
        if self.ptr == 8 && self.long == 8 {
            b.d("_LP64", "1");
            b.d("__LP64__", "1");
        } else if self.ptr == 4 {
            b.d("_ILP32", "1");
            b.d("__ILP32__", "1");
        }
        for (n, v) in [
            ("__ATOMIC_RELAXED", "0"),
            ("__ATOMIC_CONSUME", "1"),
            ("__ATOMIC_ACQUIRE", "2"),
            ("__ATOMIC_RELEASE", "3"),
            ("__ATOMIC_ACQ_REL", "4"),
            ("__ATOMIC_SEQ_CST", "5"),
            ("__MEMORY_SCOPE_SYSTEM", "0"),
            ("__MEMORY_SCOPE_DEVICE", "1"),
            ("__MEMORY_SCOPE_WRKGRP", "2"),
            ("__MEMORY_SCOPE_WVFRNT", "3"),
            ("__MEMORY_SCOPE_SINGLE", "4"),
            ("__OPENCL_MEMORY_SCOPE_WORK_ITEM", "0"),
            ("__OPENCL_MEMORY_SCOPE_WORK_GROUP", "1"),
            ("__OPENCL_MEMORY_SCOPE_DEVICE", "2"),
            ("__OPENCL_MEMORY_SCOPE_ALL_SVM_DEVICES", "3"),
            ("__OPENCL_MEMORY_SCOPE_SUB_GROUP", "4"),
            ("__FPCLASS_SNAN", "0x0001"),
            ("__FPCLASS_QNAN", "0x0002"),
            ("__FPCLASS_NEGINF", "0x0004"),
            ("__FPCLASS_NEGNORMAL", "0x0008"),
            ("__FPCLASS_NEGSUBNORMAL", "0x0010"),
            ("__FPCLASS_NEGZERO", "0x0020"),
            ("__FPCLASS_POSZERO", "0x0040"),
            ("__FPCLASS_POSSUBNORMAL", "0x0080"),
            ("__FPCLASS_POSNORMAL", "0x0100"),
            ("__FPCLASS_POSINF", "0x0200"),
        ] {
            b.d(n, v);
        }
        if gnu {
            for t in [
                "BOOL", "CHAR", "CHAR16_T", "CHAR32_T", "WCHAR_T", "SHORT", "INT", "LONG", "LLONG",
                "POINTER",
            ] {
                b.d(&format!("__GCC_ATOMIC_{t}_LOCK_FREE"), self.lock_free(t));
            }
            b.d("__GCC_ATOMIC_TEST_AND_SET_TRUEVAL", "1");
        }
        for t in [
            "BOOL", "CHAR", "CHAR16_T", "CHAR32_T", "WCHAR_T", "SHORT", "INT", "LONG", "LLONG",
            "POINTER",
        ] {
            b.d(&format!("__CLANG_ATOMIC_{t}_LOCK_FREE"), self.lock_free(t));
        }
        for n in [1, 2, 4, 8] {
            if n <= 4 || self.ptr == 8 || self.arch != Arch::Riscv32 {
                b.d(&format!("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_{n}"), "1");
            }
        }
        if matches!(self.arch, Arch::Aarch64) {
            b.d("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_16", "1");
        }

        // Sizes.
        b.d("__CHAR_BIT__", "8");
        b.d("__BOOL_WIDTH__", "1");
        b.d("__SHRT_WIDTH__", "16");
        b.d("__INT_WIDTH__", "32");
        b.d("__LONG_WIDTH__", &(self.long * 8).to_string());
        b.d("__LLONG_WIDTH__", "64");
        b.d(
            "__BITINT_MAXWIDTH__",
            if matches!(self.arch, Arch::X86_64 | Arch::X86) {
                "8388608"
            } else {
                "128"
            },
        );
        b.d("__SCHAR_MAX__", "127");
        b.d("__SHRT_MAX__", "32767");
        b.d("__INT_MAX__", "2147483647");
        b.d("__LONG_MAX__", &self.max(Prim::Long));
        b.d("__LONG_LONG_MAX__", "9223372036854775807LL");
        let size_width = |b: &mut Defs, name: &str, p: Prim| {
            b.d(&format!("{name}_MAX__"), &self.max(p));
            b.d(&format!("{name}_WIDTH__"), &self.bits(p).to_string());
        };
        size_width(&mut b, "__WCHAR", self.wchar);
        size_width(&mut b, "__WINT", self.wint);
        size_width(&mut b, "__INTMAX", self.intmax);
        size_width(&mut b, "__SIZE", self.size_t);
        size_width(&mut b, "__UINTMAX", self.intmax.to_unsigned());
        size_width(&mut b, "__PTRDIFF", self.ptrdiff_t);
        size_width(&mut b, "__INTPTR", self.intptr);
        size_width(&mut b, "__UINTPTR", self.intptr.to_unsigned());
        let sz = |p: Prim| self.prim(p).0.to_string();
        b.d("__SIZEOF_DOUBLE__", "8");
        b.d("__SIZEOF_FLOAT__", "4");
        b.d("__SIZEOF_INT__", "4");
        b.d("__SIZEOF_LONG__", &sz(Prim::Long));
        b.d("__SIZEOF_LONG_DOUBLE__", &sz(Prim::LongDouble));
        b.d("__SIZEOF_LONG_LONG__", "8");
        b.d("__SIZEOF_POINTER__", &self.ptr.to_string());
        b.d("__SIZEOF_SHORT__", "2");
        b.d("__SIZEOF_PTRDIFF_T__", &sz(self.ptrdiff_t));
        b.d("__SIZEOF_SIZE_T__", &sz(self.size_t));
        b.d("__SIZEOF_WCHAR_T__", &sz(self.wchar));
        b.d("__SIZEOF_WINT_T__", &sz(self.wint));
        if self.int128 {
            b.d("__SIZEOF_INT128__", "16");
        }
        if matches!(self.arch, Arch::X86_64 | Arch::X86) && !ms && !darwin {
            b.d("__SIZEOF_FLOAT128__", "16");
        }
        if matches!(self.arch, Arch::X86_64 | Arch::X86) && !ms && !darwin
            || self.arch == Arch::Wasm32
        {
            b.d("__FLOAT128__", "1");
        }
        let ty = |b: &mut Defs, name: &str, p: Prim| b.d(name, Self::int_name(p));
        let fmt = |b: &mut Defs, name: &str, p: Prim| {
            let m = Self::fmt_mod(p);
            let letters: &[char] = if self.signed(p) {
                &['d', 'i']
            } else {
                &['o', 'u', 'x', 'X']
            };
            for c in letters {
                b.d(&format!("{name}_FMT{c}__"), &format!("\"{m}{c}\""));
            }
        };
        ty(&mut b, "__INTMAX_TYPE__", self.intmax);
        fmt(&mut b, "__INTMAX", self.intmax);
        b.d("__INTMAX_C_SUFFIX__", Self::suffix(self.intmax));
        b.d(
            "__INTMAX_C(c)",
            &format!("c##{}", Self::suffix(self.intmax)),
        );
        let uintmax = self.intmax.to_unsigned();
        ty(&mut b, "__UINTMAX_TYPE__", uintmax);
        fmt(&mut b, "__UINTMAX", uintmax);
        b.d("__UINTMAX_C_SUFFIX__", Self::suffix(uintmax));
        b.d("__UINTMAX_C(c)", &format!("c##{}", Self::suffix(uintmax)));
        ty(&mut b, "__PTRDIFF_TYPE__", self.ptrdiff_t);
        fmt(&mut b, "__PTRDIFF", self.ptrdiff_t);
        ty(&mut b, "__INTPTR_TYPE__", self.intptr);
        fmt(&mut b, "__INTPTR", self.intptr);
        ty(&mut b, "__SIZE_TYPE__", self.size_t);
        fmt(&mut b, "__SIZE", self.size_t);
        ty(&mut b, "__WCHAR_TYPE__", self.wchar);
        ty(&mut b, "__WINT_TYPE__", self.wint);
        b.d(
            "__SIG_ATOMIC_MAX__",
            if self.arch == Arch::Wasm32 {
                "2147483647L"
            } else {
                "2147483647"
            },
        );
        b.d("__SIG_ATOMIC_WIDTH__", "32");
        b.d("__CHAR16_TYPE__", "unsigned short");
        b.d("__CHAR32_TYPE__", "unsigned int");
        let uintptr = self.intptr.to_unsigned();
        ty(&mut b, "__UINTPTR_TYPE__", uintptr);
        fmt(&mut b, "__UINTPTR", uintptr);
        b.d("__POINTER_WIDTH__", &(self.ptr * 8).to_string());
        b.d("__BIGGEST_ALIGNMENT__", &self.max_align.to_string());
        if !self.char_signed {
            b.d("__CHAR_UNSIGNED__", "1");
        }
        if !self.signed(self.wchar) {
            b.d("__WCHAR_UNSIGNED__", "1");
        }
        if !self.signed(self.wint) {
            b.d("__WINT_UNSIGNED__", "1");
        }
        // Exact, least and fast width types.
        for (w, s) in [
            (8, Prim::SChar),
            (16, Prim::Short),
            (32, Prim::Int),
            (64, self.int64),
        ] {
            for signed in [true, false] {
                let p = if signed { s } else { s.to_unsigned() };
                let pre = if signed { "__INT" } else { "__UINT" };
                ty(&mut b, &format!("{pre}{w}_TYPE__"), p);
                fmt(&mut b, &format!("{pre}{w}"), p);
                let suf = match p {
                    Prim::SChar | Prim::UChar | Prim::Short | Prim::UShort | Prim::Int => "",
                    _ => Self::suffix(p),
                };
                b.d(&format!("{pre}{w}_C_SUFFIX__"), suf);
                b.d(
                    &format!("{pre}{w}_C(c)"),
                    &if suf.is_empty() {
                        "c".to_owned()
                    } else {
                        format!("c##{suf}")
                    },
                );
                b.d(&format!("{pre}{w}_MAX__"), &self.max(p));
                for kind in ["LEAST", "FAST"] {
                    let n = format!("{pre}_{kind}{w}");
                    ty(&mut b, &format!("{n}_TYPE__"), p);
                    b.d(&format!("{n}_MAX__"), &self.max(p));
                    if signed {
                        b.d(&format!("{n}_WIDTH__"), &w.to_string());
                    }
                    fmt(&mut b, &n, p);
                }
            }
        }
        // Floating point.
        if !matches!(self.arch, Arch::Ppc64le | Arch::Wasm32) {
            float_macros(&mut b, "FLT16", Fmt::Half, "F16");
        }
        float_macros(&mut b, "FLT", Fmt::Single, "F");
        float_macros(&mut b, "DBL", Fmt::Double, "");
        let ld = match self.ld {
            LdFormat::Double => Fmt::Double,
            LdFormat::X87 => Fmt::X87,
            LdFormat::Quad => Fmt::Quad,
            LdFormat::IbmDouble => Fmt::Ibm,
        };
        float_macros(&mut b, "LDBL", ld, "L");
        b.d("__FLT_RADIX__", "2");
        b.d("__DECIMAL_DIG__", "__LDBL_DECIMAL_DIG__");
        b.d("__FLT_EVAL_METHOD__", "0");
        if !ms && !darwin {
            b.d("__USER_LABEL_PREFIX__", "");
        } else {
            b.d(
                "__USER_LABEL_PREFIX__",
                if self.underscore { "_" } else { "" },
            );
        }
        b.d("__GCC_CONSTRUCTIVE_SIZE", self.cache_line().0);
        b.d("__GCC_DESTRUCTIVE_SIZE", self.cache_line().1);
        self.os_macros(&mut b);
        self.arch_macros(&mut b);
        b.0
    }

    fn lock_free(&self, t: &str) -> &'static str {
        if t == "LLONG" && self.arch == Arch::Riscv32 {
            "1"
        } else {
            "2"
        }
    }

    const fn cache_line(&self) -> (&'static str, &'static str) {
        match self.arch {
            Arch::Aarch64 => ("64", "256"),
            Arch::Riscv64 | Arch::Riscv32 => ("32", "32"),
            Arch::Ppc64le => ("128", "128"),
            Arch::S390x => ("256", "256"),
            _ => ("64", "64"),
        }
    }

    fn os_macros(&self, b: &mut Defs) {
        let elf = matches!(self.os, Os::Linux | Os::FreeBsd)
            || (self.os == Os::None && self.arch != Arch::Wasm32);
        if elf {
            b.d("__ELF__", "1");
        }
        match self.os {
            Os::Linux => {
                for n in ["__linux", "__linux__", "linux"] {
                    b.d(n, "1");
                }
                if self.android != 0 {
                    b.d("__ANDROID__", "1");
                    b.d("__ANDROID_API__", &self.android.to_string());
                    b.d("__ANDROID_MIN_SDK_VERSION__", &self.android.to_string());
                } else {
                    b.d("__gnu_linux__", "1");
                }
                for n in ["__unix", "__unix__", "unix"] {
                    b.d(n, "1");
                }
                for n in ["__pic__", "__PIC__", "__pie__", "__PIE__"] {
                    b.d(n, "2");
                }
                if self.musl {
                    b.d("__musl__", "1");
                    b.d("__NO_MATH_ERRNO__", "1");
                }
            }
            Os::FreeBsd => {
                b.d("__FreeBSD__", "8");
                b.d("__FreeBSD_cc_version", "800001");
                b.d("__KPRINTF_ATTRIBUTE__", "1");
                b.d("__NO_MATH_ERRNO__", "1");
                for n in ["__unix", "__unix__", "unix"] {
                    b.d(n, "1");
                }
            }
            Os::Darwin => {
                b.d("__APPLE__", "1");
                b.d("__APPLE_CC__", "6000");
                b.d("__apple_build_version__", "21000334");
                b.d("__MACH__", "1");
                b.d("__DYNAMIC__", "1");
                b.d("__pic__", "2");
                b.d("__PIC__", "2");
                b.d("__SSP__", "1");
                b.d("__NO_MATH_ERRNO__", "1");
                b.d("__BLOCKS__", "1");
                b.d("__block", "__attribute__((__blocks__(byref)))");
                if self.objc {
                    // Read as ARC does: the qualifiers are ownership.
                    b.d("__OBJC__", "1");
                    b.d("__OBJC2__", "1");
                    b.d("__weak", "__attribute__((objc_ownership(weak)))");
                    b.d("__strong", "__attribute__((objc_ownership(strong)))");
                    b.d(
                        "__autoreleasing",
                        "__attribute__((objc_ownership(autoreleasing)))",
                    );
                    b.d(
                        "__unsafe_unretained",
                        "__attribute__((objc_ownership(none)))",
                    );
                    // Interface Builder's markers, as clang predefines them.
                    b.d("IBOutlet", "");
                    b.d("IBAction", "void");
                    b.d("IBInspectable", "");
                    b.d("IB_DESIGNABLE", "");
                    b.d("OBJC_NEW_PROPERTIES", "1");
                } else {
                    b.d("__weak", "__attribute__((objc_gc(weak)))");
                    b.d("__strong", "");
                    b.d("__unsafe_unretained", "");
                }
                b.d("__nonnull", "_Nonnull");
                b.d("__null_unspecified", "_Null_unspecified");
                b.d("__nullable", "_Nullable");
                b.d("_HAS_BUILTIN_AVAILABLE_FEATS", "1");
                let v = self.darwin_version.to_string();
                b.d(
                    if self.ios {
                        "__ENVIRONMENT_IPHONE_OS_VERSION_MIN_REQUIRED__"
                    } else {
                        "__ENVIRONMENT_MAC_OS_X_VERSION_MIN_REQUIRED__"
                    },
                    &v,
                );
                b.d("__ENVIRONMENT_OS_VERSION_MIN_REQUIRED__", &v);
                let flag = |on: bool| if on { "1" } else { "0" };
                let (mac, ios, sim) = (flag(!self.ios), flag(self.ios), flag(self.sim));
                let embedded = flag(self.ios && !self.sim);
                for (n, v) in [
                    ("TARGET_OS_OSX", mac),
                    ("TARGET_OS_IPHONE", ios),
                    ("TARGET_OS_IOS", ios),
                    ("TARGET_OS_SIMULATOR", sim),
                    ("TARGET_IPHONE_SIMULATOR", sim),
                    ("TARGET_OS_EMBEDDED", embedded),
                ] {
                    b.d(n, v);
                }
                for (n, v) in [
                    ("TARGET_OS_MAC", "1"),
                    ("TARGET_OS_ARROW", "1"),
                    ("TARGET_OS_TV", "0"),
                    ("TARGET_OS_WATCH", "0"),
                    ("TARGET_OS_VISION", "0"),
                    ("TARGET_OS_XR", "0"),
                    ("TARGET_OS_DRIVERKIT", "0"),
                    ("TARGET_OS_MACCATALYST", "0"),
                    ("TARGET_OS_UIKITFORMAC", "0"),
                    ("TARGET_OS_IOSMAC", "0"),
                    ("TARGET_OS_NANO", "0"),
                    ("TARGET_OS_BRIDGE", "0"),
                    ("TARGET_OS_FIRMWARE", "0"),
                    ("TARGET_OS_KERNELKIT", "0"),
                    ("TARGET_OS_LINUX", "0"),
                    ("TARGET_OS_UEFI", "0"),
                    ("TARGET_OS_UNIX", "0"),
                    ("TARGET_OS_WIN32", "0"),
                    ("TARGET_OS_WINDOWS", "0"),
                ] {
                    b.d(n, v);
                }
            }
            Os::Windows => {
                b.d("_WIN32", "1");
                if self.ptr == 8 {
                    b.d("_WIN64", "1");
                }
                b.d("_MSC_VER", "1933");
                b.d("_MSC_FULL_VER", "193300000");
                b.d("_MSC_BUILD", "1");
                b.d("_MSC_EXTENSIONS", "1");
                b.d("_MSVC_CONSTEXPR_ATTRIBUTE", "1");
                b.d("_CRT_USE_BUILTIN_OFFSETOF", "1");
                b.d("_M_FP_CONTRACT", "1");
                b.d("_M_FP_PRECISE", "1");
                b.d("_INTEGRAL_MAX_BITS", "64");
                b.d("_MSVC_EXECUTION_CHARACTER_SET", "65001");
                match self.arch {
                    Arch::X86_64 => {
                        b.d("_M_X64", "100");
                        b.d("_M_AMD64", "100");
                    }
                    Arch::X86 => {
                        b.d("_M_IX86", "600");
                        b.d("_M_IX86_FP", "2");
                    }
                    _ => {
                        b.d("_M_ARM64", "1");
                        b.d("_ISO_VOLATILE", "1");
                    }
                }
                if self.arch != Arch::X86 {
                    b.d("__pic__", "2");
                    b.d("__PIC__", "2");
                }
            }
            Os::Wasi => {
                b.d("__wasi__", "1");
                b.d("__NO_MATH_ERRNO__", "1");
            }
            Os::None => {
                if self.arch == Arch::Wasm32 {
                    b.d("__NO_MATH_ERRNO__", "1");
                }
            }
        }
    }

    fn arch_macros(&self, b: &mut Defs) {
        let gnu = !self.msvc;
        match self.arch {
            Arch::X86_64 | Arch::X86 => {
                let x64 = self.arch == Arch::X86_64;
                if x64 {
                    for n in ["__x86_64", "__x86_64__", "__amd64", "__amd64__"] {
                        b.d(n, "1");
                    }
                    for n in ["__k8", "__k8__", "__tune_k8__"] {
                        b.d(n, "1");
                    }
                } else {
                    for n in ["__i386", "__i386__", "i386"] {
                        b.d(n, "1");
                    }
                    for n in [
                        "__pentium4",
                        "__pentium4__",
                        "__tune_pentium4__",
                        "__LAHF_SAHF__",
                    ] {
                        b.d(n, "1");
                    }
                }
                b.d("__code_model_small__", "1");
                b.d("__REGISTER_PREFIX__", "");
                for n in [
                    "__MMX__",
                    "__SSE__",
                    "__SSE2__",
                    "__SSE_MATH__",
                    "__SSE2_MATH__",
                    "__FXSR__",
                    "__SEG_FS",
                    "__SEG_GS",
                    "__GCC_ASM_FLAG_OUTPUTS__",
                    "__NO_MATH_INLINES",
                ] {
                    b.d(n, "1");
                }
                b.d("__seg_fs", "__attribute__((address_space(257)))");
                b.d("__seg_gs", "__attribute__((address_space(256)))");
                if gnu {
                    b.d("__GCC_HAVE_DWARF2_CFI_ASM", "1");
                }
            }
            Arch::Aarch64 => {
                for n in [
                    "__aarch64__",
                    "__AARCH64EL__",
                    "__AARCH64_CMODEL_SMALL__",
                    "__ARM_64BIT_STATE",
                    "__ARM_ARCH_ISA_A64",
                    "__ARM_FEATURE_CLZ",
                    "__ARM_FEATURE_DIRECTED_ROUNDING",
                    "__ARM_FEATURE_DIV",
                    "__ARM_FEATURE_FMA",
                    "__ARM_FEATURE_IDIV",
                    "__ARM_FEATURE_NUMERIC_MAXMIN",
                    "__ARM_FEATURE_UNALIGNED",
                    "__ARM_FP16_ARGS",
                    "__ARM_FP16_FORMAT_IEEE",
                    "__ARM_NEON",
                    "__ARM_NEON_SVE_BRIDGE",
                    "__ARM_PCS_AAPCS64",
                    "__ARM_STATE_ZA",
                    "__ARM_STATE_ZT0",
                    "__FP_FAST_FMA",
                    "__FP_FAST_FMAF",
                    "__GCC_ASM_FLAG_OUTPUTS__",
                ] {
                    b.d(n, "1");
                }
                b.d("__ARM_ACLE", "202420");
                b.d(
                    "__ARM_ACLE_VERSION(year,quarter,patch)",
                    "(100 * (year) + 10 * (quarter) + (patch))",
                );
                b.d("__FUNCTION_MULTI_VERSIONING_SUPPORT_LEVEL", "202430");
                b.d("__ARM_ALIGN_MAX_STACK_PWR", "4");
                b.d("__ARM_ARCH", "8");
                b.d("__ARM_ARCH_PROFILE", "'A'");
                b.d("__ARM_FEATURE_LDREX", "0xF");
                b.d("__ARM_FP", "0xE");
                b.d("__ARM_NEON_FP", "0xE");
                b.d("__ARM_SIZEOF_MINIMAL_ENUM", "4");
                b.d("__ARM_SIZEOF_WCHAR_T", "4");
                if gnu {
                    b.d("__GCC_HAVE_DWARF2_CFI_ASM", "1");
                }
                if self.os == Os::Darwin {
                    b.d("__REGISTER_PREFIX__", "");
                    for n in [
                        "__HAVE_FUNCTION_MULTI_VERSIONING",
                        "__ARM_FEATURE_ATOMICS",
                        "__ARM_FEATURE_FRINT",
                        "__AARCH64_SIMD__",
                        "__ARM64_ARCH_8__",
                        "__ARM_NEON__",
                        "__arm64",
                        "__arm64__",
                        "__ARM_ARCH_8_2__",
                        "__ARM_ARCH_8_3__",
                        "__ARM_ARCH_8_4__",
                        "__ARM_ARCH_8_5__",
                        "__ARM_FEATURE_AES",
                        "__ARM_FEATURE_BTI",
                        "__ARM_FEATURE_COMPLEX",
                        "__ARM_FEATURE_CRC32",
                        "__ARM_FEATURE_CRYPTO",
                        "__ARM_FEATURE_DOTPROD",
                        "__ARM_FEATURE_FP16_FML",
                        "__ARM_FEATURE_FP16_SCALAR_ARITHMETIC",
                        "__ARM_FEATURE_FP16_VECTOR_ARITHMETIC",
                        "__ARM_FEATURE_JCVT",
                        "__ARM_FEATURE_PAUTH",
                        "__ARM_FEATURE_QRDMX",
                        "__ARM_FEATURE_RCPC",
                        "__ARM_FEATURE_SHA2",
                        "__ARM_FEATURE_SHA3",
                        "__ARM_FEATURE_SHA512",
                    ] {
                        b.d(n, "1");
                    }
                }
            }
            Arch::Arm => {
                for n in [
                    "__arm",
                    "__arm__",
                    "__APCS_32__",
                    "__ARM_32BIT_STATE",
                    "__ARM_ARCH_7A__",
                    "__ARM_ARCH_ISA_ARM",
                    "__ARM_EABI__",
                    "__ARM_FEATURE_CLZ",
                    "__ARM_FEATURE_DSP",
                    "__ARM_FEATURE_QBIT",
                    "__ARM_FEATURE_SAT",
                    "__ARM_FEATURE_SIMD32",
                    "__ARM_FEATURE_UNALIGNED",
                    "__ARM_FP16_ARGS",
                    "__ARM_FP16_FORMAT_IEEE",
                    "__ARM_NEON",
                    "__ARM_NEON__",
                    "__ARM_PCS",
                    "__ARM_PCS_VFP",
                    "__ARM_VFPV2__",
                    "__ARM_VFPV3__",
                    "__ARMEL__",
                    "__VFP_FP__",
                    "__THUMB_INTERWORK__",
                ] {
                    b.d(n, "1");
                }
                b.d("__ARM_ACLE", "200");
                b.d("__ARM_ARCH", "7");
                b.d("__ARM_ARCH_ISA_THUMB", "2");
                b.d("__ARM_ARCH_PROFILE", "'A'");
                b.d("__ARM_FEATURE_COPROC", "0xf");
                b.d("__ARM_FEATURE_LDREX", "0xf");
                b.d("__ARM_FP", "0xc");
                b.d("__ARM_NEON_FP", "0x4");
                b.d("__ARM_SIZEOF_MINIMAL_ENUM", "4");
                b.d("__ARM_SIZEOF_WCHAR_T", "4");
                b.d("__REGISTER_PREFIX__", "");
            }
            Arch::Riscv64 | Arch::Riscv32 => {
                let rv64 = self.arch == Arch::Riscv64;
                b.d("__riscv", "1");
                b.d("__riscv_xlen", if rv64 { "64" } else { "32" });
                for (n, v) in [
                    ("__riscv_a", "2001000"),
                    ("__riscv_arch_test", "1"),
                    ("__riscv_atomic", "1"),
                    ("__riscv_c", "2000000"),
                    ("__riscv_cmodel_medlow", "1"),
                    ("__riscv_compressed", "1"),
                    ("__riscv_div", "1"),
                    ("__riscv_i", "2001000"),
                    ("__riscv_m", "2000000"),
                    ("__riscv_misaligned_avoid", "1"),
                    ("__riscv_mul", "1"),
                    ("__riscv_muldiv", "1"),
                    ("__riscv_v_intrinsic", "1000000"),
                    ("__riscv_zaamo", "1000000"),
                    ("__riscv_zalrsc", "1000000"),
                    ("__riscv_zca", "1000000"),
                    ("__riscv_zmmul", "1000000"),
                ] {
                    b.d(n, v);
                }
                if rv64 {
                    for (n, v) in [
                        ("__riscv_d", "2002000"),
                        ("__riscv_f", "2002000"),
                        ("__riscv_fdiv", "1"),
                        ("__riscv_flen", "64"),
                        ("__riscv_float_abi_double", "1"),
                        ("__riscv_fsqrt", "1"),
                        ("__riscv_zcd", "1000000"),
                        ("__riscv_zicsr", "2000000"),
                        ("__GCC_HAVE_DWARF2_CFI_ASM", "1"),
                    ] {
                        b.d(n, v);
                    }
                } else {
                    b.d("__riscv_float_abi_soft", "1");
                }
            }
            Arch::Ppc64le => {
                for n in [
                    "__powerpc__",
                    "__POWERPC__",
                    "__powerpc64__",
                    "__ppc__",
                    "__PPC__",
                    "__PPC64__",
                    "_ARCH_PPC",
                    "_ARCH_PPC64",
                    "_ARCH_PPCGR",
                    "_ARCH_PPCSQ",
                    "_ARCH_PWR4",
                    "_ARCH_PWR5",
                    "_ARCH_PWR5X",
                    "_ARCH_PWR6",
                    "_ARCH_PWR7",
                    "_ARCH_PWR8",
                    "_CALL_LINUX",
                    "__ALTIVEC__",
                    "__VSX__",
                    "__POWER8_VECTOR__",
                    "__CRYPTO__",
                    "__HTM__",
                    "__HAVE_BSWAP__",
                    "__NATURAL_ALIGNMENT__",
                    "__RSQRTE__",
                    "__RSQRTEF__",
                    "__LONG_DOUBLE_128__",
                    "__LONG_DOUBLE_IBM128__",
                    "__LONGDOUBLE128",
                    "__GCC_HAVE_DWARF2_CFI_ASM",
                ] {
                    b.d(n, "1");
                }
                b.d("_CALL_ELF", "2");
                b.d("__VEC__", "10206");
                b.d("__STRUCT_PARM_ALIGN__", "16");
                b.d("__REGISTER_PREFIX__", "");
                b.d("_LITTLE_ENDIAN", "1");
            }
            Arch::S390x => {
                for n in ["__s390__", "__s390x__", "__zarch__", "__LONG_DOUBLE_128__"] {
                    b.d(n, "1");
                }
                b.d("__ARCH__", "8");
            }
            Arch::Wasm32 => {
                for n in [
                    "__wasm",
                    "__wasm__",
                    "__wasm32",
                    "__wasm32__",
                    "__wasm_bulk_memory__",
                    "__wasm_bulk_memory_opt__",
                    "__wasm_multivalue__",
                    "__wasm_mutable_globals__",
                    "__wasm_nontrapping_fptoint__",
                    "__wasm_reference_types__",
                    "__wasm_sign_ext__",
                ] {
                    b.d(n, "1");
                }
                if self.triple.contains("threads") {
                    b.d("__wasm_atomics__", "1");
                }
            }
        }
    }
}

struct Defs(String);

impl Defs {
    fn d(&mut self, name: &str, value: &str) {
        let _ = writeln!(self.0, "#define {name} {value}");
    }
}

#[derive(Clone, Copy)]
enum Fmt {
    Half,
    Single,
    Double,
    X87,
    Quad,
    Ibm,
}

fn float_macros(b: &mut Defs, pre: &str, f: Fmt, ext: &str) {
    #[rustfmt::skip]
    let v: [&str; 15] = match f {
        Fmt::Half => ["5", "5.9604644775390625e-8", "3", "9.765625e-4", "11", "6.5504e+4", "4",
            "16", "6.103515625e-5", "(-4)", "(-13)", "6.5504e+4", "1", "1", "1"],
        Fmt::Single => ["9", "1.40129846e-45", "6", "1.19209290e-7", "24", "3.40282347e+38",
            "38", "128", "1.17549435e-38", "(-37)", "(-125)", "3.40282347e+38", "1", "1", "1"],
        Fmt::Double => ["17", "4.9406564584124654e-324", "15", "2.2204460492503131e-16", "53",
            "1.7976931348623157e+308", "308", "1024", "2.2250738585072014e-308", "(-307)",
            "(-1021)", "1.7976931348623157e+308", "1", "1", "1"],
        Fmt::X87 => ["21", "3.64519953188247460253e-4951", "18", "1.08420217248550443401e-19",
            "64", "1.18973149535723176502e+4932", "4932", "16384",
            "3.36210314311209350626e-4932", "(-4931)", "(-16381)",
            "1.18973149535723176502e+4932", "1", "1", "1"],
        Fmt::Quad => ["36", "6.47517511943802511092443895822764655e-4966", "33",
            "1.92592994438723585305597794258492732e-34", "113",
            "1.18973149535723176508575932662800702e+4932", "4932", "16384",
            "3.36210314311209350626267781732175260e-4932", "(-4931)", "(-16381)",
            "1.18973149535723176508575932662800702e+4932", "1", "1", "1"],
        Fmt::Ibm => ["33", "4.94065645841246544176568792868221e-324", "31",
            "4.94065645841246544176568792868221e-324", "106",
            "1.79769313486231580793728971405301e+308", "308", "1024",
            "2.00416836000897277799610805135016e-292", "(-291)", "(-968)",
            "8.98846567431157953864652595394501e+307", "1", "1", "1"],
    };
    let names = [
        "DECIMAL_DIG",
        "DENORM_MIN",
        "DIG",
        "EPSILON",
        "MANT_DIG",
        "MAX",
        "MAX_10_EXP",
        "MAX_EXP",
        "MIN",
        "MIN_10_EXP",
        "MIN_EXP",
        "NORM_MAX",
        "HAS_DENORM",
        "HAS_INFINITY",
        "HAS_QUIET_NAN",
    ];
    for (n, val) in names.iter().zip(v) {
        let floaty = matches!(*n, "DENORM_MIN" | "EPSILON" | "MAX" | "MIN" | "NORM_MAX");
        let val = if floaty {
            format!("{val}{ext}")
        } else {
            val.to_owned()
        };
        b.d(&format!("__{pre}_{n}__"), &val);
    }
}

#[cfg(test)]
mod tests {
    use super::{Target, TargetInfo};

    fn apple(triple: &str, os: &str) -> String {
        Target::new(&TargetInfo {
            triple,
            arch: "aarch64",
            os,
            env: "",
            vendor: "apple",
            pointer_width: 64,
            big_endian: false,
        })
        .unwrap()
        .predefines()
    }

    /// The SDK headers' `TargetConditionals.h` view: macOS, an iOS
    /// device, the iOS simulator.
    #[test]
    fn apple_platform_macros() {
        let mac = apple("arm64-apple-macosx11.0", "macos");
        let dev = apple("arm64-apple-ios15.0", "ios");
        let sim = apple("arm64-apple-ios16.1-simulator", "ios");
        for (p, want) in [
            (&mac, "TARGET_OS_OSX 1"),
            (&mac, "TARGET_OS_IOS 0"),
            (&mac, "__ENVIRONMENT_MAC_OS_X_VERSION_MIN_REQUIRED__ 110000"),
            (&dev, "TARGET_OS_OSX 0"),
            (&dev, "TARGET_OS_IPHONE 1"),
            (&dev, "TARGET_OS_SIMULATOR 0"),
            (&dev, "TARGET_OS_EMBEDDED 1"),
            (
                &dev,
                "__ENVIRONMENT_IPHONE_OS_VERSION_MIN_REQUIRED__ 150000",
            ),
            (&sim, "TARGET_OS_SIMULATOR 1"),
            (&sim, "TARGET_OS_EMBEDDED 0"),
            (&sim, "__ENVIRONMENT_OS_VERSION_MIN_REQUIRED__ 160100"),
        ] {
            assert!(p.contains(&format!("#define {want}\n")), "{want}");
        }
        assert!(!dev.contains("MAC_OS_X_VERSION_MIN"));
    }
}
