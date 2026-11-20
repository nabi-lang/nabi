/*===---- pmmintrin.h - SSE3 intrinsics ------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* pmmintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __PMMINTRIN_H
#define __PMMINTRIN_H
#if !defined(__i386__) && !defined(__x86_64__)
#error "This header is only meant to be used on x86 and x64 architecture"
#endif
#include <emmintrin.h>
__m128i _mm_lddqu_si128(__m128i_u const *__p);
__m128 _mm_addsub_ps(__m128 __a, __m128 __b);
__m128 _mm_hadd_ps(__m128 __a, __m128 __b);
__m128 _mm_hsub_ps(__m128 __a, __m128 __b);
__m128 _mm_movehdup_ps(__m128 __a);
__m128 _mm_moveldup_ps(__m128 __a);
__m128d _mm_addsub_pd(__m128d __a, __m128d __b);
__m128d _mm_hadd_pd(__m128d __a, __m128d __b);
__m128d _mm_hsub_pd(__m128d __a, __m128d __b);
__m128d _mm_movedup_pd(__m128d __a);
void _mm_monitor(void const *__p, unsigned __extensions, unsigned __hints);
void _mm_mwait(unsigned __extensions, unsigned __hints);
#endif
