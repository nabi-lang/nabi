/*===---- bmiintrin.h - BMI intrinsics -------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* bmiintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#if !defined __X86INTRIN_H && !defined __IMMINTRIN_H
#error "Never use <bmiintrin.h> directly; include <x86intrin.h> instead."
#endif
#ifndef __BMIINTRIN_H
#define __BMIINTRIN_H
unsigned short __tzcnt_u16(unsigned short __X);
#define _tzcnt_u16 __tzcnt_u16
unsigned int __tzcnt_u32(unsigned int __X);
int _mm_tzcnt_32(unsigned int __X);
#define _tzcnt_u32 __tzcnt_u32
#ifdef __x86_64__
unsigned long long __tzcnt_u64(unsigned long long __X);
long long _mm_tzcnt_64(unsigned long long __X);
#define _tzcnt_u64 __tzcnt_u64
#endif
unsigned int __andn_u32(unsigned int __X, unsigned int __Y);
#define _andn_u32 __andn_u32
unsigned int __bextr_u32(unsigned int __X, unsigned int __Y);
unsigned int _bextr_u32(unsigned int __X, unsigned int __Y, unsigned int __Z);
unsigned int _bextr2_u32(unsigned int __X, unsigned int __Y);
unsigned int __blsi_u32(unsigned int __X);
#define _blsi_u32 __blsi_u32
unsigned int __blsmsk_u32(unsigned int __X);
#define _blsmsk_u32 __blsmsk_u32
unsigned int __blsr_u32(unsigned int __X);
#define _blsr_u32 __blsr_u32
#ifdef __x86_64__
unsigned long long __andn_u64 (unsigned long long __X, unsigned long long __Y);
#define _andn_u64 __andn_u64
unsigned long long __bextr_u64(unsigned long long __X, unsigned long long __Y);
unsigned long long _bextr_u64(unsigned long long __X, unsigned int __Y, unsigned int __Z);
unsigned long long _bextr2_u64(unsigned long long __X, unsigned long long __Y);
unsigned long long __blsi_u64(unsigned long long __X);
#define _blsi_u64 __blsi_u64
unsigned long long __blsmsk_u64(unsigned long long __X);
#define _blsmsk_u64 __blsmsk_u64
unsigned long long __blsr_u64(unsigned long long __X);
#define _blsr_u64 __blsr_u64
#endif
#endif
