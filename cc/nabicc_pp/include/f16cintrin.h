/*===---- f16cintrin.h - F16C intrinsics -----------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* f16cintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#if !defined __IMMINTRIN_H
#error "Never use <f16cintrin.h> directly; include <immintrin.h> instead."
#endif
#ifndef __F16CINTRIN_H
#define __F16CINTRIN_H
float _cvtsh_ss(unsigned short __a);
__m128 _mm_cvtph_ps(__m128i __a);
__m256 _mm256_cvtph_ps(__m128i __a);
#endif
