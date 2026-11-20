/*===---- crc32intrin.h - SSE4.2 Accumulate CRC32 intrinsics ---------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* crc32intrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __CRC32INTRIN_H
#define __CRC32INTRIN_H
unsigned int _mm_crc32_u8(unsigned int __C, unsigned char __D);
unsigned int _mm_crc32_u16(unsigned int __C, unsigned short __D);
unsigned int _mm_crc32_u32(unsigned int __C, unsigned int __D);
#ifdef __x86_64__
unsigned long long _mm_crc32_u64(unsigned long long __C, unsigned long long __D);
#endif
#endif
