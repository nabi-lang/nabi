/*===---- arm_acle.h - ARM Non-Neon intrinsics -----------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 * The Arm C Language Extensions specifications can be found in the following
 * link: https://github.com/ARM-software/acle/releases
 *
 * The ACLE section numbers are subject to change. When consulting the
 * specifications, it is recommended to search using section titles if
 * the section numbers look outdated.
 *
 *===-----------------------------------------------------------------------===
 */
/* arm_acle.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the Arm intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __ARM_ACLE_H
#define __ARM_ACLE_H
#ifndef __ARM_ACLE
#error "ACLE intrinsics support not enabled."
#endif
#include <stdint.h>
void __dmb(unsigned int);
void __dsb(unsigned int);
void __isb(unsigned int);
void __wfi(void);
void __wfe(void);
void __sev(void);
void __sevl(void);
void __yield(void);
#if defined(__ARM_64BIT_STATE) && __ARM_64BIT_STATE
#define _CHKFEAT_GCS 1
uint64_t __chkfeat(uint64_t __features);
#endif
uint32_t __swp(uint32_t __x, volatile uint32_t *__p);
#if !defined(_MSC_VER) || (!defined(__aarch64__) && !defined(__arm64ec__))
void __nop(void);
#endif
uint32_t __ror(uint32_t __x, uint32_t __y);
uint64_t __rorll(uint64_t __x, uint32_t __y);
unsigned long __rorl(unsigned long __x, uint32_t __y);
unsigned int __clz(uint32_t __t);
unsigned int __clzl(unsigned long __t);
unsigned int __clzll(uint64_t __t);
unsigned int __cls(uint32_t __t);
unsigned int __clsl(unsigned long __t);
unsigned int __clsll(uint64_t __t);
uint32_t __rev(uint32_t __t);
unsigned long __revl(unsigned long __t);
uint64_t __revll(uint64_t __t);
uint32_t __rev16(uint32_t __t);
uint64_t __rev16ll(uint64_t __t);
unsigned long __rev16l(unsigned long __t);
int16_t __revsh(int16_t __t);
uint32_t __rbit(uint32_t __t);
uint64_t __rbitll(uint64_t __t);
unsigned long __rbitl(unsigned long __t);
#if defined(__ARM_32BIT_STATE) && __ARM_32BIT_STATE
int32_t __smulbb(int32_t __a, int32_t __b);
int32_t __smulbt(int32_t __a, int32_t __b);
int32_t __smultb(int32_t __a, int32_t __b);
int32_t __smultt(int32_t __a, int32_t __b);
int32_t __smulwb(int32_t __a, int32_t __b);
int32_t __smulwt(int32_t __a, int32_t __b);
#endif
#if defined(__ARM_32BIT_STATE) && __ARM_32BIT_STATE
int32_t __qadd(int32_t __t, int32_t __v);
int32_t __qsub(int32_t __t, int32_t __v);
int32_t __qdbl(int32_t __t);
#endif
#if defined(__ARM_32BIT_STATE) && __ARM_32BIT_STATE
int32_t __smlabb(int32_t __a, int32_t __b, int32_t __c);
int32_t __smlabt(int32_t __a, int32_t __b, int32_t __c);
int32_t __smlatb(int32_t __a, int32_t __b, int32_t __c);
int32_t __smlatt(int32_t __a, int32_t __b, int32_t __c);
int32_t __smlawb(int32_t __a, int32_t __b, int32_t __c);
int32_t __smlawt(int32_t __a, int32_t __b, int32_t __c);
#endif
#if defined(__ARM_FEATURE_SIMD32) && __ARM_FEATURE_SIMD32
typedef int32_t int8x4_t;
typedef int32_t int16x2_t;
typedef uint32_t uint8x4_t;
typedef uint32_t uint16x2_t;
int16x2_t __sxtab16(int16x2_t __a, int8x4_t __b);
int16x2_t __sxtb16(int8x4_t __a);
int16x2_t __uxtab16(int16x2_t __a, int8x4_t __b);
int16x2_t __uxtb16(int8x4_t __a);
#endif
#if defined(__ARM_FEATURE_SIMD32) && __ARM_FEATURE_SIMD32
uint8x4_t __sel(uint8x4_t __a, uint8x4_t __b);
#endif
#if defined(__ARM_FEATURE_SIMD32) && __ARM_FEATURE_SIMD32
int8x4_t __qadd8(int8x4_t __a, int8x4_t __b);
int8x4_t __qsub8(int8x4_t __a, int8x4_t __b);
int8x4_t __sadd8(int8x4_t __a, int8x4_t __b);
int8x4_t __shadd8(int8x4_t __a, int8x4_t __b);
int8x4_t __shsub8(int8x4_t __a, int8x4_t __b);
int8x4_t __ssub8(int8x4_t __a, int8x4_t __b);
uint8x4_t __uadd8(uint8x4_t __a, uint8x4_t __b);
uint8x4_t __uhadd8(uint8x4_t __a, uint8x4_t __b);
uint8x4_t __uhsub8(uint8x4_t __a, uint8x4_t __b);
uint8x4_t __uqadd8(uint8x4_t __a, uint8x4_t __b);
uint8x4_t __uqsub8(uint8x4_t __a, uint8x4_t __b);
uint8x4_t __usub8(uint8x4_t __a, uint8x4_t __b);
#endif
#if defined(__ARM_FEATURE_SIMD32) && __ARM_FEATURE_SIMD32
uint32_t __usad8(uint8x4_t __a, uint8x4_t __b);
uint32_t __usada8(uint8x4_t __a, uint8x4_t __b, uint32_t __c);
#endif
#if defined(__ARM_FEATURE_SIMD32) && __ARM_FEATURE_SIMD32
int16x2_t __qadd16(int16x2_t __a, int16x2_t __b);
int16x2_t __qasx(int16x2_t __a, int16x2_t __b);
int16x2_t __qsax(int16x2_t __a, int16x2_t __b);
int16x2_t __qsub16(int16x2_t __a, int16x2_t __b);
int16x2_t __sadd16(int16x2_t __a, int16x2_t __b);
int16x2_t __sasx(int16x2_t __a, int16x2_t __b);
int16x2_t __shadd16(int16x2_t __a, int16x2_t __b);
int16x2_t __shasx(int16x2_t __a, int16x2_t __b);
int16x2_t __shsax(int16x2_t __a, int16x2_t __b);
int16x2_t __shsub16(int16x2_t __a, int16x2_t __b);
int16x2_t __ssax(int16x2_t __a, int16x2_t __b);
int16x2_t __ssub16(int16x2_t __a, int16x2_t __b);
uint16x2_t __uadd16(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uasx(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uhadd16(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uhasx(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uhsax(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uhsub16(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uqadd16(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uqasx(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uqsax(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __uqsub16(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __usax(uint16x2_t __a, uint16x2_t __b);
uint16x2_t __usub16(uint16x2_t __a, uint16x2_t __b);
#endif
#if defined(__ARM_FEATURE_SIMD32) && __ARM_FEATURE_SIMD32
int32_t __smlad(int16x2_t __a, int16x2_t __b, int32_t __c);
int32_t __smladx(int16x2_t __a, int16x2_t __b, int32_t __c);
int64_t __smlald(int16x2_t __a, int16x2_t __b, int64_t __c);
int64_t __smlaldx(int16x2_t __a, int16x2_t __b, int64_t __c);
int32_t __smlsd(int16x2_t __a, int16x2_t __b, int32_t __c);
int32_t __smlsdx(int16x2_t __a, int16x2_t __b, int32_t __c);
int64_t __smlsld(int16x2_t __a, int16x2_t __b, int64_t __c);
int64_t __smlsldx(int16x2_t __a, int16x2_t __b, int64_t __c);
int32_t __smuad(int16x2_t __a, int16x2_t __b);
int32_t __smuadx(int16x2_t __a, int16x2_t __b);
int32_t __smusd(int16x2_t __a, int16x2_t __b);
int32_t __smusdx(int16x2_t __a, int16x2_t __b);
#endif
#if (defined(__ARM_FEATURE_DIRECTED_ROUNDING)    &&                            (__ARM_FEATURE_DIRECTED_ROUNDING))             &&                            (defined(__ARM_64BIT_STATE) && __ARM_64BIT_STATE)
double __rintn(double __a);
float __rintnf(float __a);
#endif
uint32_t __crc32b(uint32_t __a, uint8_t __b);
uint32_t __crc32h(uint32_t __a, uint16_t __b);
uint32_t __crc32w(uint32_t __a, uint32_t __b);
uint32_t __crc32d(uint32_t __a, uint64_t __b);
uint32_t __crc32cb(uint32_t __a, uint8_t __b);
uint32_t __crc32ch(uint32_t __a, uint16_t __b);
uint32_t __crc32cw(uint32_t __a, uint32_t __b);
uint32_t __crc32cd(uint32_t __a, uint64_t __b);
#if defined(__ARM_64BIT_STATE) && __ARM_64BIT_STATE
int32_t __jcvt(double __a);
#endif
#if defined(__ARM_64BIT_STATE) && __ARM_64BIT_STATE
float __rint32zf(float __a);
double __rint32z(double __a);
float __rint64zf(float __a);
double __rint64z(double __a);
float __rint32xf(float __a);
double __rint32x(double __a);
float __rint64xf(float __a);
double __rint64x(double __a);
#endif
#if defined(__ARM_64BIT_STATE) && __ARM_64BIT_STATE
typedef struct { uint64_t val[8]; } data512_t;
data512_t __arm_ld64b(const void *__addr);
void __arm_st64b(void *__addr, data512_t __value);
uint64_t __arm_st64bv(void *__addr, data512_t __value);
uint64_t __arm_st64bv0(void *__addr, data512_t __value);
#endif
#if defined(__ARM_FEATURE_TME) && __ARM_FEATURE_TME
#define _TMFAILURE_REASON  0x00007fffu
#define _TMFAILURE_RTRY    0x00008000u
#define _TMFAILURE_CNCL    0x00010000u
#define _TMFAILURE_MEM     0x00020000u
#define _TMFAILURE_IMP     0x00040000u
#define _TMFAILURE_ERR     0x00080000u
#define _TMFAILURE_SIZE    0x00100000u
#define _TMFAILURE_NEST    0x00200000u
#define _TMFAILURE_DBG     0x00400000u
#define _TMFAILURE_INT     0x00800000u
#define _TMFAILURE_TRIVIAL 0x01000000u
#endif
#if defined(__ARM_64BIT_STATE) && __ARM_64BIT_STATE
int __rndr(uint64_t *__p);
int __rndrrs(uint64_t *__p);
#endif
#if defined(__ARM_64BIT_STATE) && __ARM_64BIT_STATE
void * __gcspr();
uint64_t __gcspopm();
void * __gcsss(void *__stack);
#endif
#endif
