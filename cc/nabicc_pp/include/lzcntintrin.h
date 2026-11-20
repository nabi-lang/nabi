/*===---- lzcntintrin.h - LZCNT intrinsics ---------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* lzcntintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#if !defined __X86INTRIN_H && !defined __IMMINTRIN_H
#error "Never use <lzcntintrin.h> directly; include <x86intrin.h> instead."
#endif
#ifndef __LZCNTINTRIN_H
#define __LZCNTINTRIN_H
unsigned int __lzcnt32(unsigned int __X);
unsigned int _lzcnt_u32(unsigned int __X);
#ifdef __x86_64__
unsigned long long _lzcnt_u64(unsigned long long __X);
#endif
#endif
