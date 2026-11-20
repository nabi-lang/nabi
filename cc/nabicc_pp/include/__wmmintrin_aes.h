/*===---- __wmmintrin_aes.h - AES intrinsics -------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* __wmmintrin_aes.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __WMMINTRIN_H
#error "Never use <__wmmintrin_aes.h> directly; include <wmmintrin.h> instead."
#endif
#ifndef __WMMINTRIN_AES_H
#define __WMMINTRIN_AES_H
__m128i _mm_aesenc_si128(__m128i __V, __m128i __R);
__m128i _mm_aesenclast_si128(__m128i __V, __m128i __R);
__m128i _mm_aesdec_si128(__m128i __V, __m128i __R);
__m128i _mm_aesdeclast_si128(__m128i __V, __m128i __R);
__m128i _mm_aesimc_si128(__m128i __V);
#endif
