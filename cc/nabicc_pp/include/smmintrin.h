/*===---- smmintrin.h - SSE4 intrinsics ------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* smmintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __SMMINTRIN_H
#define __SMMINTRIN_H
#if !defined(__i386__) && !defined(__x86_64__)
#error "This header is only meant to be used on x86 and x64 architecture"
#endif
#include <tmmintrin.h>
#define _MM_FROUND_TO_NEAREST_INT 0x00
#define _MM_FROUND_TO_NEG_INF 0x01
#define _MM_FROUND_TO_POS_INF 0x02
#define _MM_FROUND_TO_ZERO 0x03
#define _MM_FROUND_CUR_DIRECTION 0x04
#define _MM_FROUND_RAISE_EXC 0x00
#define _MM_FROUND_NO_EXC 0x08
#define _MM_FROUND_NINT (_MM_FROUND_RAISE_EXC | _MM_FROUND_TO_NEAREST_INT)
#define _MM_FROUND_FLOOR (_MM_FROUND_RAISE_EXC | _MM_FROUND_TO_NEG_INF)
#define _MM_FROUND_CEIL (_MM_FROUND_RAISE_EXC | _MM_FROUND_TO_POS_INF)
#define _MM_FROUND_TRUNC (_MM_FROUND_RAISE_EXC | _MM_FROUND_TO_ZERO)
#define _MM_FROUND_RINT (_MM_FROUND_RAISE_EXC | _MM_FROUND_CUR_DIRECTION)
#define _MM_FROUND_NEARBYINT (_MM_FROUND_NO_EXC | _MM_FROUND_CUR_DIRECTION)
__m128d _mm_blendv_pd(__m128d __V1, __m128d __V2, __m128d __M);
__m128 _mm_blendv_ps(__m128 __V1, __m128 __V2, __m128 __M);
__m128i _mm_blendv_epi8(__m128i __V1, __m128i __V2, __m128i __M);
__m128i _mm_mullo_epi32(__m128i __V1, __m128i __V2);
__m128i _mm_mul_epi32(__m128i __V1, __m128i __V2);
__m128i _mm_stream_load_si128(const void *__V);
__m128i _mm_min_epi8(__m128i __V1, __m128i __V2);
__m128i _mm_max_epi8(__m128i __V1, __m128i __V2);
__m128i _mm_min_epu16(__m128i __V1, __m128i __V2);
__m128i _mm_max_epu16(__m128i __V1, __m128i __V2);
__m128i _mm_min_epi32(__m128i __V1, __m128i __V2);
__m128i _mm_max_epi32(__m128i __V1, __m128i __V2);
__m128i _mm_min_epu32(__m128i __V1, __m128i __V2);
__m128i _mm_max_epu32(__m128i __V1, __m128i __V2);
#define _MM_EXTRACT_FLOAT(D, X, N)                                                do {                                                                              (D) = __builtin_ia32_vec_ext_v4sf((__v4sf)(__m128)(X), (int)(N));             } while (0)
#define _MM_MK_INSERTPS_NDX(X, Y, Z) (((X) << 6) | ((Y) << 4) | (Z))
#define _MM_PICK_OUT_PS(X, N)                                                     _mm_insert_ps(_mm_setzero_ps(), (X), _MM_MK_INSERTPS_NDX((N), 0, 0x0e))
int _mm_testz_si128(__m128i __M, __m128i __V);
int _mm_testc_si128(__m128i __M, __m128i __V);
int _mm_testnzc_si128(__m128i __M, __m128i __V);
__m128i _mm_cmpeq_epi64(__m128i __V1, __m128i __V2);
__m128i _mm_cvtepi8_epi16(__m128i __V);
__m128i _mm_cvtepi8_epi32(__m128i __V);
__m128i _mm_cvtepi8_epi64(__m128i __V);
__m128i _mm_cvtepi16_epi32(__m128i __V);
__m128i _mm_cvtepi16_epi64(__m128i __V);
__m128i _mm_cvtepi32_epi64(__m128i __V);
__m128i _mm_cvtepu8_epi16(__m128i __V);
__m128i _mm_cvtepu8_epi32(__m128i __V);
__m128i _mm_cvtepu8_epi64(__m128i __V);
__m128i _mm_cvtepu16_epi32(__m128i __V);
__m128i _mm_cvtepu16_epi64(__m128i __V);
__m128i _mm_cvtepu32_epi64(__m128i __V);
__m128i _mm_packus_epi32(__m128i __V1, __m128i __V2);
__m128i _mm_minpos_epu16(__m128i __V);
#define _SIDD_UBYTE_OPS 0x00
#define _SIDD_UWORD_OPS 0x01
#define _SIDD_SBYTE_OPS 0x02
#define _SIDD_SWORD_OPS 0x03
#define _SIDD_CMP_EQUAL_ANY 0x00
#define _SIDD_CMP_RANGES 0x04
#define _SIDD_CMP_EQUAL_EACH 0x08
#define _SIDD_CMP_EQUAL_ORDERED 0x0c
#define _SIDD_POSITIVE_POLARITY 0x00
#define _SIDD_NEGATIVE_POLARITY 0x10
#define _SIDD_MASKED_POSITIVE_POLARITY 0x20
#define _SIDD_MASKED_NEGATIVE_POLARITY 0x30
#define _SIDD_LEAST_SIGNIFICANT 0x00
#define _SIDD_MOST_SIGNIFICANT 0x40
#define _SIDD_BIT_MASK 0x00
#define _SIDD_UNIT_MASK 0x40
__m128i _mm_cmpgt_epi64(__m128i __V1, __m128i __V2);
#include <popcntintrin.h>
#include <crc32intrin.h>
#endif
