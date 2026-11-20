/*===---- adxintrin.h - ADX intrinsics -------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* adxintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __IMMINTRIN_H
#error "Never use <adxintrin.h> directly; include <immintrin.h> instead."
#endif
#ifndef __ADXINTRIN_H
#define __ADXINTRIN_H
unsigned char _addcarryx_u32(unsigned char __cf, unsigned int __x, unsigned int __y, unsigned int *__p);
#ifdef __x86_64__
unsigned char _addcarryx_u64(unsigned char __cf, unsigned long long __x, unsigned long long __y, unsigned long long *__p);
#endif
#endif
