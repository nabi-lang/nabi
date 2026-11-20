/*===---- popcntintrin.h - POPCNT intrinsics -------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* popcntintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __POPCNTINTRIN_H
#define __POPCNTINTRIN_H
int _mm_popcnt_u32(unsigned int __A);
#ifdef __x86_64__
long long _mm_popcnt_u64(unsigned long long __A);
#endif
#endif
