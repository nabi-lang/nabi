//! libm behind `nabi_`-prefixed symbols for core's `f32` / `f64` methods.
//! core cannot declare `floor` / `log` itself: an
//! extern's name is global in nabi, so a user `fn log` would clash. The
//! driver links `-lm` (the UCRT on Windows). `abs` / `copysign` / `min`
//! / `max` are Rust's `core` (bit operations; the UCRT does not export
//! every `f32` form on every architecture).

use nabir_abi::half;

macro_rules! libm {
    ($($nabi:ident = $c:ident($($a:ident),+) : $t:ty;)*) => {
        unsafe extern "C" {
            $(fn $c($($a: $t),+) -> $t;)*
        }
        $(
            #[unsafe(no_mangle)]
            pub extern "C" fn $nabi($($a: $t),+) -> $t {
                // SAFETY: libm math functions are pure on their arguments.
                unsafe { $c($($a),+) }
            }
        )*
    };
}

// C89's `float` forms: x86's UCRT has none (MSVC's headers widen them
// to the `double` ones, as here).
macro_rules! libm_f32 {
    ($($nabi:ident = $c:ident / $d:ident($($a:ident),+);)*) => {
        #[cfg(not(all(windows, target_arch = "x86")))]
        libm! { $($nabi = $c($($a),+): f32;)* }
        $(
            #[cfg(all(windows, target_arch = "x86"))]
            #[unsafe(no_mangle)]
            pub extern "C" fn $nabi($($a: f32),+) -> f32 {
                unsafe extern "C" {
                    fn $d($($a: f64),+) -> f64;
                }
                // SAFETY: as in `libm!`.
                unsafe { $d($(f64::from($a)),+) as f32 }
            }
        )*
    };
}

libm_f32! {
    nabi_f32_floor = floorf / floor(x);
    nabi_f32_ceil = ceilf / ceil(x);
    nabi_f32_exp = expf / exp(x);
    nabi_f32_ln = logf / log(x);
    nabi_f32_log10 = log10f / log10(x);
    nabi_f32_sin = sinf / sin(x);
    nabi_f32_cos = cosf / cos(x);
    nabi_f32_tan = tanf / tan(x);
    nabi_f32_asin = asinf / asin(x);
    nabi_f32_acos = acosf / acos(x);
    nabi_f32_atan = atanf / atan(x);
    nabi_f32_sinh = sinhf / sinh(x);
    nabi_f32_cosh = coshf / cosh(x);
    nabi_f32_tanh = tanhf / tanh(x);
    nabi_f32_atan2 = atan2f / atan2(x, y);
}

libm! {
    nabi_f64_floor = floor(x): f64;
    nabi_f64_ceil = ceil(x): f64;
    nabi_f64_round = round(x): f64;
    nabi_f64_trunc = trunc(x): f64;
    nabi_f64_cbrt = cbrt(x): f64;
    nabi_f64_exp = exp(x): f64;
    nabi_f64_exp2 = exp2(x): f64;
    nabi_f64_exp_m1 = expm1(x): f64;
    nabi_f64_ln = log(x): f64;
    nabi_f64_ln_1p = log1p(x): f64;
    nabi_f64_log2 = log2(x): f64;
    nabi_f64_log10 = log10(x): f64;
    nabi_f64_sin = sin(x): f64;
    nabi_f64_cos = cos(x): f64;
    nabi_f64_tan = tan(x): f64;
    nabi_f64_asin = asin(x): f64;
    nabi_f64_acos = acos(x): f64;
    nabi_f64_atan = atan(x): f64;
    nabi_f64_sinh = sinh(x): f64;
    nabi_f64_cosh = cosh(x): f64;
    nabi_f64_tanh = tanh(x): f64;
    nabi_f64_asinh = asinh(x): f64;
    nabi_f64_acosh = acosh(x): f64;
    nabi_f64_atanh = atanh(x): f64;
    nabi_f64_pow = pow(x, y): f64;
    nabi_f64_atan2 = atan2(x, y): f64;
    nabi_f64_hypot = hypot(x, y): f64;
    nabi_f64_mul_add = fma(x, y, z): f64;

    nabi_f32_round = roundf(x): f32;
    nabi_f32_trunc = truncf(x): f32;
    nabi_f32_cbrt = cbrtf(x): f32;
    nabi_f32_exp2 = exp2f(x): f32;
    nabi_f32_exp_m1 = expm1f(x): f32;
    nabi_f32_ln_1p = log1pf(x): f32;
    nabi_f32_log2 = log2f(x): f32;
    nabi_f32_asinh = asinhf(x): f32;
    nabi_f32_acosh = acoshf(x): f32;
    nabi_f32_atanh = atanhf(x): f32;
    nabi_f32_pow = powf(x, y): f32;
    nabi_f32_mul_add = fmaf(x, y, z): f32;
}

// `powi`, which `core` cannot reach: compiler-builtins' functions
// (repeated multiplication, no libm call).
unsafe extern "C" {
    fn __powidf2(a: f64, b: i32) -> f64;
    fn __powisf2(a: f32, b: i32) -> f32;
}
#[unsafe(no_mangle)]
pub extern "C" fn nabi_f64_powi(x: f64, n: i32) -> f64 {
    // SAFETY: pure on its arguments.
    unsafe { __powidf2(x, n) }
}
#[unsafe(no_mangle)]
pub extern "C" fn nabi_f32_powi(x: f32, n: i32) -> f32 {
    // SAFETY: pure on its arguments.
    unsafe { __powisf2(x, n) }
}

// Through `hypot`: exact in `f64`, so rounding it gives `hypotf`'s
// result (the UCRT has only `_hypotf`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_f32_hypot(x: f32, y: f32) -> f32 {
    nabi_f64_hypot(f64::from(x), f64::from(y)) as f32
}

// `min` / `max` ignore a NaN operand, as C's `fmin` / `fmax`.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_f64_abs(x: f64) -> f64 {
    x.abs()
}
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_f64_copysign(x: f64, y: f64) -> f64 {
    x.copysign(y)
}
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_f64_min(x: f64, y: f64) -> f64 {
    nabir_abi::float::min_f64(x, y)
}
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_f64_max(x: f64, y: f64) -> f64 {
    nabir_abi::float::max_f64(x, y)
}
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_f32_abs(x: f32) -> f32 {
    x.abs()
}
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_f32_copysign(x: f32, y: f32) -> f32 {
    x.copysign(y)
}
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_f32_min(x: f32, y: f32) -> f32 {
    nabir_abi::float::min_f32(x, y)
}
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_f32_max(x: f32, y: f32) -> f32 {
    nabir_abi::float::max_f32(x, y)
}

// `f16` / `bf16` held as their bits (`nabir_abi::half`). Codegen does
// `nabi_f16_from_f32` / `nabi_f16_to_f32` inline where the CPU converts
// (x86-64 F16C, arm64).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_f16_from_f32(x: f32) -> u16 {
    half::from_f32(half::F16, x)
}
#[unsafe(no_mangle)]
pub extern "C" fn nabi_f16_to_f32(h: u16) -> f32 {
    half::to_f32(half::F16, h)
}
#[unsafe(no_mangle)]
pub extern "C" fn nabi_f16_from_f64(x: f64) -> u16 {
    half::from_f64(half::F16, x)
}
#[unsafe(no_mangle)]
pub extern "C" fn nabi_bf16_from_f64(x: f64) -> u16 {
    half::from_f64(half::BF16, x)
}
#[unsafe(no_mangle)]
pub extern "C" fn nabi_bf16_from_i64(x: i64) -> u16 {
    half::from_int(half::BF16, x < 0, x.unsigned_abs())
}
#[unsafe(no_mangle)]
pub extern "C" fn nabi_bf16_from_u64(x: u64) -> u16 {
    half::from_int(half::BF16, false, x)
}

// libgcc's / compiler-rt's `__bf16` narrowing, which compiler_builtins
// lacks: clang keeps a C `__bf16` widened to `float` across blocks and
// narrows it back with `__truncsfbf2`, so a C library's object needs it
// to link. The `__bf16` result is its bits in the low half of the `float`
// return register (x86-64 xmm0, arm64 v0, arm s0, RISC-V fa0 or a0).
// ELF targets only, where the corpora checked them; there
// localize.sh makes them weak, so libgcc's copy wins where linked.
#[cfg(all(
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "arm",
        target_arch = "riscv64",
        target_arch = "riscv32"
    ),
    not(any(windows, target_vendor = "apple"))
))]
#[unsafe(no_mangle)]
pub extern "C" fn __truncsfbf2(x: f32) -> f32 {
    f32::from_bits(u32::from(half::from_f32(half::BF16, x)))
}
#[cfg(all(
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "arm",
        target_arch = "riscv64",
        target_arch = "riscv32"
    ),
    not(any(windows, target_vendor = "apple"))
))]
#[unsafe(no_mangle)]
pub extern "C" fn __truncdfbf2(x: f64) -> f32 {
    f32::from_bits(u32::from(half::from_f64(half::BF16, x)))
}

// i386 returns a `__bf16` in xmm0 and a `float` on the x87 stack: the
// bits come from these, then move to xmm0.
#[cfg(all(target_arch = "x86", not(windows)))]
extern "C" fn bf16_from_f32(x: f32) -> u16 {
    half::from_f32(half::BF16, x)
}
#[cfg(all(target_arch = "x86", not(windows)))]
extern "C" fn bf16_from_f64(x: f64) -> u16 {
    half::from_f64(half::BF16, x)
}
#[cfg(all(target_arch = "x86", not(windows)))]
core::arch::global_asm!(
    ".globl __truncsfbf2",
    ".type __truncsfbf2, @function",
    "__truncsfbf2:",
    "push dword ptr [esp + 4]",
    "call {sf}",
    "add esp, 4",
    "movd xmm0, eax",
    "ret",
    ".size __truncsfbf2, . - __truncsfbf2",
    ".globl __truncdfbf2",
    ".type __truncdfbf2, @function",
    "__truncdfbf2:",
    "push dword ptr [esp + 8]",
    "push dword ptr [esp + 8]",
    "call {df}",
    "add esp, 8",
    "movd xmm0, eax",
    "ret",
    ".size __truncdfbf2, . - __truncdfbf2",
    sf = sym bf16_from_f32,
    df = sym bf16_from_f64,
);

#[cfg(all(
    test,
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "arm",
        target_arch = "riscv64",
        target_arch = "riscv32"
    ),
    not(any(windows, target_vendor = "apple"))
))]
mod tests {
    use super::{__truncdfbf2, __truncsfbf2};

    #[test]
    fn bf16_truncations_round_to_nearest_even() {
        let bits = |f: f32| f.to_bits();
        assert_eq!(bits(__truncsfbf2(1.0)), 0x3f80);
        // Halfway: ties to even, down from 0x3f80 and up from 0x3f81.
        assert_eq!(bits(__truncsfbf2(f32::from_bits(0x3f80_8000))), 0x3f80);
        assert_eq!(bits(__truncsfbf2(f32::from_bits(0x3f81_8000))), 0x3f82);
        assert_eq!(bits(__truncsfbf2(-0.0)), 0x8000);
        assert_eq!(bits(__truncsfbf2(f32::INFINITY)), 0x7f80);
        assert_eq!(bits(__truncsfbf2(f32::NAN)) & 0x7fc0, 0x7fc0);
        assert_eq!(bits(__truncdfbf2(1.0)), 0x3f80);
        assert_eq!(bits(__truncdfbf2(-2.5)), 0xc020);
        assert_eq!(bits(__truncdfbf2(1e300)), 0x7f80);
    }
}
