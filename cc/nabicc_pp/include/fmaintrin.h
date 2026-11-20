/*===---- fmaintrin.h - FMA intrinsics -------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* fmaintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __IMMINTRIN_H
#error "Never use <fmaintrin.h> directly; include <immintrin.h> instead."
#endif
#ifndef __FMAINTRIN_H
#define __FMAINTRIN_H
__m128 _mm_fmadd_ps(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fmadd_pd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fmadd_ss(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fmadd_sd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fmsub_ps(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fmsub_pd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fmsub_ss(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fmsub_sd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fnmadd_ps(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fnmadd_pd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fnmadd_ss(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fnmadd_sd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fnmsub_ps(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fnmsub_pd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fnmsub_ss(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fnmsub_sd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fmaddsub_ps(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fmaddsub_pd(__m128d __A, __m128d __B, __m128d __C);
__m128 _mm_fmsubadd_ps(__m128 __A, __m128 __B, __m128 __C);
__m128d _mm_fmsubadd_pd(__m128d __A, __m128d __B, __m128d __C);
__m256 _mm256_fmadd_ps(__m256 __A, __m256 __B, __m256 __C);
__m256d _mm256_fmadd_pd(__m256d __A, __m256d __B, __m256d __C);
__m256 _mm256_fmsub_ps(__m256 __A, __m256 __B, __m256 __C);
__m256d _mm256_fmsub_pd(__m256d __A, __m256d __B, __m256d __C);
__m256 _mm256_fnmadd_ps(__m256 __A, __m256 __B, __m256 __C);
__m256d _mm256_fnmadd_pd(__m256d __A, __m256d __B, __m256d __C);
__m256 _mm256_fnmsub_ps(__m256 __A, __m256 __B, __m256 __C);
__m256d _mm256_fnmsub_pd(__m256d __A, __m256d __B, __m256d __C);
__m256 _mm256_fmaddsub_ps(__m256 __A, __m256 __B, __m256 __C);
__m256d _mm256_fmaddsub_pd(__m256d __A, __m256d __B, __m256d __C);
__m256 _mm256_fmsubadd_ps(__m256 __A, __m256 __B, __m256 __C);
__m256d _mm256_fmsubadd_pd(__m256d __A, __m256d __B, __m256d __C);
#endif
