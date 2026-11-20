/*===---- bmi2intrin.h - BMI2 intrinsics -----------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* bmi2intrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __IMMINTRIN_H
#error "Never use <bmi2intrin.h> directly; include <immintrin.h> instead."
#endif
#ifndef __BMI2INTRIN_H
#define __BMI2INTRIN_H
unsigned int _bzhi_u32(unsigned int __X, unsigned int __Y);
unsigned int _pdep_u32(unsigned int __X, unsigned int __Y);
unsigned int _pext_u32(unsigned int __X, unsigned int __Y);
unsigned int _mulx_u32(unsigned int __X, unsigned int __Y, unsigned int *__P);
#ifdef  __x86_64__
unsigned long long _bzhi_u64(unsigned long long __X, unsigned long long __Y);
unsigned long long _pdep_u64(unsigned long long __X, unsigned long long __Y);
unsigned long long _pext_u64(unsigned long long __X, unsigned long long __Y);
unsigned long long _mulx_u64 (unsigned long long __X, unsigned long long __Y, unsigned long long *__P);
#endif
#endif
