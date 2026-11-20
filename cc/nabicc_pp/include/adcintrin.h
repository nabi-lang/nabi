/*===---- adcintrin.h - ADC intrinsics -------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* adcintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __ADCINTRIN_H
#define __ADCINTRIN_H
#if !defined(__i386__) && !defined(__x86_64__)
#error "This header is only meant to be used on x86 and x64 architecture"
#endif
unsigned char _addcarry_u32(unsigned char __cf, unsigned int __x, unsigned int __y, unsigned int *__p);
unsigned char _subborrow_u32(unsigned char __cf, unsigned int __x, unsigned int __y, unsigned int *__p);
#ifdef __x86_64__
unsigned char _addcarry_u64(unsigned char __cf, unsigned long long __x, unsigned long long __y, unsigned long long *__p);
unsigned char _subborrow_u64(unsigned char __cf, unsigned long long __x, unsigned long long __y, unsigned long long *__p);
#endif
#endif
