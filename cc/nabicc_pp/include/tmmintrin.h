/*===---- tmmintrin.h - SSSE3 intrinsics -----------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* tmmintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __TMMINTRIN_H
#define __TMMINTRIN_H
#if !defined(__i386__) && !defined(__x86_64__)
#error "This header is only meant to be used on x86 and x64 architecture"
#endif
#include <pmmintrin.h>
__m64 _mm_abs_pi8(__m64 __a);
__m128i _mm_abs_epi8(__m128i __a);
__m64 _mm_abs_pi16(__m64 __a);
__m128i _mm_abs_epi16(__m128i __a);
__m64 _mm_abs_pi32(__m64 __a);
__m128i _mm_abs_epi32(__m128i __a);
__m128i _mm_hadd_epi16(__m128i __a, __m128i __b);
__m128i _mm_hadd_epi32(__m128i __a, __m128i __b);
__m64 _mm_hadd_pi16(__m64 __a, __m64 __b);
__m64 _mm_hadd_pi32(__m64 __a, __m64 __b);
__m128i _mm_hadds_epi16(__m128i __a, __m128i __b);
__m64 _mm_hadds_pi16(__m64 __a, __m64 __b);
__m128i _mm_hsub_epi16(__m128i __a, __m128i __b);
__m128i _mm_hsub_epi32(__m128i __a, __m128i __b);
__m64 _mm_hsub_pi16(__m64 __a, __m64 __b);
__m64 _mm_hsub_pi32(__m64 __a, __m64 __b);
__m128i _mm_hsubs_epi16(__m128i __a, __m128i __b);
__m64 _mm_hsubs_pi16(__m64 __a, __m64 __b);
__m128i _mm_maddubs_epi16(__m128i __a, __m128i __b);
__m64 _mm_maddubs_pi16(__m64 __a, __m64 __b);
__m128i _mm_mulhrs_epi16(__m128i __a, __m128i __b);
__m64 _mm_mulhrs_pi16(__m64 __a, __m64 __b);
__m128i _mm_shuffle_epi8(__m128i __a, __m128i __b);
__m64 _mm_shuffle_pi8(__m64 __a, __m64 __b);
__m128i _mm_sign_epi8(__m128i __a, __m128i __b);
__m128i _mm_sign_epi16(__m128i __a, __m128i __b);
__m128i _mm_sign_epi32(__m128i __a, __m128i __b);
__m64 _mm_sign_pi8(__m64 __a, __m64 __b);
__m64 _mm_sign_pi16(__m64 __a, __m64 __b);
__m64 _mm_sign_pi32(__m64 __a, __m64 __b);
#endif
