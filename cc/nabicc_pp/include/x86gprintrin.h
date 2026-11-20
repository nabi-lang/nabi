/*===--------------- x86gprintrin.h - X86 GPR intrinsics ------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* x86gprintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __X86GPRINTRIN_H
#define __X86GPRINTRIN_H
#include <crc32intrin.h>
#if defined(__i386__)
#define __SAVE_GPRBX "mov {%%ebx, %%eax |eax, ebx};"
#define __RESTORE_GPRBX "mov {%%eax, %%ebx |ebx, eax};"
#define __TMPGPR "eax"
#else
#define __SAVE_GPRBX "mov {%%rbx, %%rax |rax, rbx};"
#define __RESTORE_GPRBX "mov {%%rax, %%rbx |rbx, rax};"
#define __TMPGPR "rax"
#endif
#define __SSC_MARK(__Tag)                                                         __asm__ __volatile__( __SAVE_GPRBX                                                                   "mov {%0, %%ebx|ebx, %0}; "                                                     ".byte 0x64, 0x67, 0x90; "                                                       __RESTORE_GPRBX                                                                ::"i"(__Tag)                                                                    :  __TMPGPR );
#endif
