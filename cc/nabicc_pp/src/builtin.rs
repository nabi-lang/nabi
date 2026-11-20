//! The headers clang has built in (its resource directory), written for
//! this front end: they define what the target's predefined macros say.
//!
//! The intrinsic headers (`immintrin.h`, `arm_neon.h`, Microsoft's
//! `intrin.h`, ..) are clang's as declarations: prototypes without bodies,
//! the vector types and the constants. `intrin.py` regenerates them;
//! they keep LLVM's license (`include/LICENSE-LLVM.txt`).

/// The text of built-in header `name`.
pub fn header(name: &str) -> Option<&'static str> {
    Some(match name {
        "stddef.h" => include_str!("../include/stddef.h"),
        "stdarg.h" => include_str!("../include/stdarg.h"),
        "stdbool.h" => include_str!("../include/stdbool.h"),
        "stdint.h" => include_str!("../include/stdint.h"),
        "stdalign.h" => include_str!("../include/stdalign.h"),
        "stdnoreturn.h" => include_str!("../include/stdnoreturn.h"),
        "stdatomic.h" => include_str!("../include/stdatomic.h"),
        "limits.h" => include_str!("../include/limits.h"),
        "float.h" => include_str!("../include/float.h"),
        "iso646.h" => include_str!("../include/iso646.h"),
        "inttypes.h" => include_str!("../include/inttypes.h"),
        "vadefs.h" => include_str!("../include/vadefs.h"),
        "intrin.h" => include_str!("../include/intrin.h"),
        "ptrauth.h" => include_str!("../include/ptrauth.h"),
        "ptrcheck.h" => include_str!("../include/ptrcheck.h"),
        // The intrinsics, as declarations.
        "__wmmintrin_aes.h" => include_str!("../include/__wmmintrin_aes.h"),
        "__wmmintrin_pclmul.h" => include_str!("../include/__wmmintrin_pclmul.h"),
        "adcintrin.h" => include_str!("../include/adcintrin.h"),
        "adxintrin.h" => include_str!("../include/adxintrin.h"),
        "arm64intr.h" => include_str!("../include/arm64intr.h"),
        "arm_acle.h" => include_str!("../include/arm_acle.h"),
        "arm_bf16.h" => include_str!("../include/arm_bf16.h"),
        "arm_fp16.h" => include_str!("../include/arm_fp16.h"),
        "arm_neon.h" => include_str!("../include/arm_neon.h"),
        "arm_vector_types.h" => include_str!("../include/arm_vector_types.h"),
        "armintr.h" => include_str!("../include/armintr.h"),
        "avx2intrin.h" => include_str!("../include/avx2intrin.h"),
        "avxintrin.h" => include_str!("../include/avxintrin.h"),
        "bmi2intrin.h" => include_str!("../include/bmi2intrin.h"),
        "bmiintrin.h" => include_str!("../include/bmiintrin.h"),
        "crc32intrin.h" => include_str!("../include/crc32intrin.h"),
        "emmintrin.h" => include_str!("../include/emmintrin.h"),
        "f16cintrin.h" => include_str!("../include/f16cintrin.h"),
        "fmaintrin.h" => include_str!("../include/fmaintrin.h"),
        "ia32intrin.h" => include_str!("../include/ia32intrin.h"),
        "immintrin.h" => include_str!("../include/immintrin.h"),
        "intrin0.h" => include_str!("../include/intrin0.h"),
        "lzcntintrin.h" => include_str!("../include/lzcntintrin.h"),
        "mm_malloc.h" => include_str!("../include/mm_malloc.h"),
        "mmintrin.h" => include_str!("../include/mmintrin.h"),
        "nmmintrin.h" => include_str!("../include/nmmintrin.h"),
        "pmmintrin.h" => include_str!("../include/pmmintrin.h"),
        "popcntintrin.h" => include_str!("../include/popcntintrin.h"),
        "smmintrin.h" => include_str!("../include/smmintrin.h"),
        "tmmintrin.h" => include_str!("../include/tmmintrin.h"),
        "wmmintrin.h" => include_str!("../include/wmmintrin.h"),
        "x86gprintrin.h" => include_str!("../include/x86gprintrin.h"),
        "x86intrin.h" => include_str!("../include/x86intrin.h"),
        "xmmintrin.h" => include_str!("../include/xmmintrin.h"),
        _ => return None,
    })
}
