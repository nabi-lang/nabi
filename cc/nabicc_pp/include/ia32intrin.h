/* ===-------- ia32intrin.h ---------------------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* ia32intrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __X86INTRIN_H
#error "Never use <ia32intrin.h> directly; include <x86intrin.h> instead."
#endif
#ifndef __IA32INTRIN_H
#define __IA32INTRIN_H
int __bsfd(int __A);
int __bsrd(int __A);
int __bswapd(int __A);
int _bswap(int __A);
#ifdef __x86_64__
int __bsfq(long long __A);
int __bsrq(long long __A);
long long __bswapq(long long __A);
#endif
int __popcntd(unsigned int __A);
#ifdef __x86_64__
long long __popcntq(unsigned long long __A);
#endif
#ifdef __x86_64__
unsigned long long __readeflags(void);
void __writeeflags(unsigned long long __f);
#endif
unsigned int _castf32_u32(float __A);
unsigned long long _castf64_u64(double __A);
float _castu32_f32(unsigned int __A);
double _castu64_f64(unsigned long long __A);
unsigned int __crc32b(unsigned int __C, unsigned char __D);
unsigned int __crc32w(unsigned int __C, unsigned short __D);
unsigned int __crc32d(unsigned int __C, unsigned int __D);
#ifdef __x86_64__
unsigned long long __crc32q(unsigned long long __C, unsigned long long __D);
#endif
unsigned long long __rdpmc(int __A);
unsigned long long __rdtscp(unsigned int *__A);
void _wbinvd(void);
unsigned char __rolb(unsigned char __X, int __C);
unsigned char __rorb(unsigned char __X, int __C);
unsigned short __rolw(unsigned short __X, int __C);
unsigned short __rorw(unsigned short __X, int __C);
unsigned int __rold(unsigned int __X, int __C);
unsigned int __rord(unsigned int __X, int __C);
#ifdef __x86_64__
unsigned long long __rolq(unsigned long long __X, int __C);
unsigned long long __rorq(unsigned long long __X, int __C);
#endif
#endif
