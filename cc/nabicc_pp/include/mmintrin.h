/*===---- mmintrin.h - MMX intrinsics --------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* mmintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __MMINTRIN_H
#define __MMINTRIN_H
#if !defined(__i386__) && !defined(__x86_64__)
#error "This header is only meant to be used on x86 and x64 architecture"
#endif
typedef long long __m64 __attribute__((__vector_size__(8), __aligned__(8)));
typedef long long __v1di __attribute__((__vector_size__(8)));
typedef int __v2si __attribute__((__vector_size__(8)));
typedef short __v4hi __attribute__((__vector_size__(8)));
typedef char __v8qi __attribute__((__vector_size__(8)));
typedef unsigned long long __v1du __attribute__ ((__vector_size__ (8)));
typedef unsigned int __v2su __attribute__ ((__vector_size__ (8)));
typedef unsigned short __v4hu __attribute__((__vector_size__(8)));
typedef unsigned char __v8qu __attribute__((__vector_size__(8)));
typedef signed char __v8qs __attribute__((__vector_size__(8)));
typedef long long __m128i __attribute__((__vector_size__(16), __aligned__(16)));
typedef long long __v2di __attribute__ ((__vector_size__ (16)));
typedef int __v4si __attribute__((__vector_size__(16)));
typedef short __v8hi __attribute__((__vector_size__(16)));
typedef char __v16qi __attribute__((__vector_size__(16)));
void _mm_empty(void);
__m64 _mm_cvtsi32_si64(int __i);
int _mm_cvtsi64_si32(__m64 __m);
__m64 _mm_cvtsi64_m64(long long __i);
long long _mm_cvtm64_si64(__m64 __m);
__m64 _mm_packs_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_packs_pi32(__m64 __m1, __m64 __m2);
__m64 _mm_packs_pu16(__m64 __m1, __m64 __m2);
__m64 _mm_unpackhi_pi8(__m64 __m1, __m64 __m2);
__m64 _mm_unpackhi_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_unpackhi_pi32(__m64 __m1, __m64 __m2);
__m64 _mm_unpacklo_pi8(__m64 __m1, __m64 __m2);
__m64 _mm_unpacklo_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_unpacklo_pi32(__m64 __m1, __m64 __m2);
__m64 _mm_add_pi8(__m64 __m1, __m64 __m2);
__m64 _mm_add_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_add_pi32(__m64 __m1, __m64 __m2);
__m64 _mm_adds_pi8(__m64 __m1, __m64 __m2);
__m64 _mm_adds_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_adds_pu8(__m64 __m1, __m64 __m2);
__m64 _mm_adds_pu16(__m64 __m1, __m64 __m2);
__m64 _mm_sub_pi8(__m64 __m1, __m64 __m2);
__m64 _mm_sub_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_sub_pi32(__m64 __m1, __m64 __m2);
__m64 _mm_subs_pi8(__m64 __m1, __m64 __m2);
__m64 _mm_subs_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_subs_pu8(__m64 __m1, __m64 __m2);
__m64 _mm_subs_pu16(__m64 __m1, __m64 __m2);
__m64 _mm_madd_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_mulhi_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_mullo_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_sll_pi16(__m64 __m, __m64 __count);
__m64 _mm_slli_pi16(__m64 __m, int __count);
__m64 _mm_sll_pi32(__m64 __m, __m64 __count);
__m64 _mm_slli_pi32(__m64 __m, int __count);
__m64 _mm_sll_si64(__m64 __m, __m64 __count);
__m64 _mm_slli_si64(__m64 __m, int __count);
__m64 _mm_sra_pi16(__m64 __m, __m64 __count);
__m64 _mm_srai_pi16(__m64 __m, int __count);
__m64 _mm_sra_pi32(__m64 __m, __m64 __count);
__m64 _mm_srai_pi32(__m64 __m, int __count);
__m64 _mm_srl_pi16(__m64 __m, __m64 __count);
__m64 _mm_srli_pi16(__m64 __m, int __count);
__m64 _mm_srl_pi32(__m64 __m, __m64 __count);
__m64 _mm_srli_pi32(__m64 __m, int __count);
__m64 _mm_srl_si64(__m64 __m, __m64 __count);
__m64 _mm_srli_si64(__m64 __m, int __count);
__m64 _mm_and_si64(__m64 __m1, __m64 __m2);
__m64 _mm_andnot_si64(__m64 __m1, __m64 __m2);
__m64 _mm_or_si64(__m64 __m1, __m64 __m2);
__m64 _mm_xor_si64(__m64 __m1, __m64 __m2);
__m64 _mm_cmpeq_pi8(__m64 __m1, __m64 __m2);
__m64 _mm_cmpeq_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_cmpeq_pi32(__m64 __m1, __m64 __m2);
__m64 _mm_cmpgt_pi8(__m64 __m1, __m64 __m2);
__m64 _mm_cmpgt_pi16(__m64 __m1, __m64 __m2);
__m64 _mm_cmpgt_pi32(__m64 __m1, __m64 __m2);
__m64 _mm_setzero_si64(void);
__m64 _mm_set_pi32(int __i1, int __i0);
__m64 _mm_set_pi16(short __s3, short __s2, short __s1, short __s0);
__m64 _mm_set_pi8(char __b7, char __b6, char __b5, char __b4, char __b3, char __b2, char __b1, char __b0);
__m64 _mm_set1_pi32(int __i);
__m64 _mm_set1_pi16(short __w);
__m64 _mm_set1_pi8(char __b);
__m64 _mm_setr_pi32(int __i0, int __i1);
__m64 _mm_setr_pi16(short __w0, short __w1, short __w2, short __w3);
__m64 _mm_setr_pi8(char __b0, char __b1, char __b2, char __b3, char __b4, char __b5, char __b6, char __b7);
#define _m_empty _mm_empty
#define _m_from_int _mm_cvtsi32_si64
#define _m_from_int64 _mm_cvtsi64_m64
#define _m_to_int _mm_cvtsi64_si32
#define _m_to_int64 _mm_cvtm64_si64
#define _m_packsswb _mm_packs_pi16
#define _m_packssdw _mm_packs_pi32
#define _m_packuswb _mm_packs_pu16
#define _m_punpckhbw _mm_unpackhi_pi8
#define _m_punpckhwd _mm_unpackhi_pi16
#define _m_punpckhdq _mm_unpackhi_pi32
#define _m_punpcklbw _mm_unpacklo_pi8
#define _m_punpcklwd _mm_unpacklo_pi16
#define _m_punpckldq _mm_unpacklo_pi32
#define _m_paddb _mm_add_pi8
#define _m_paddw _mm_add_pi16
#define _m_paddd _mm_add_pi32
#define _m_paddsb _mm_adds_pi8
#define _m_paddsw _mm_adds_pi16
#define _m_paddusb _mm_adds_pu8
#define _m_paddusw _mm_adds_pu16
#define _m_psubb _mm_sub_pi8
#define _m_psubw _mm_sub_pi16
#define _m_psubd _mm_sub_pi32
#define _m_psubsb _mm_subs_pi8
#define _m_psubsw _mm_subs_pi16
#define _m_psubusb _mm_subs_pu8
#define _m_psubusw _mm_subs_pu16
#define _m_pmaddwd _mm_madd_pi16
#define _m_pmulhw _mm_mulhi_pi16
#define _m_pmullw _mm_mullo_pi16
#define _m_psllw _mm_sll_pi16
#define _m_psllwi _mm_slli_pi16
#define _m_pslld _mm_sll_pi32
#define _m_pslldi _mm_slli_pi32
#define _m_psllq _mm_sll_si64
#define _m_psllqi _mm_slli_si64
#define _m_psraw _mm_sra_pi16
#define _m_psrawi _mm_srai_pi16
#define _m_psrad _mm_sra_pi32
#define _m_psradi _mm_srai_pi32
#define _m_psrlw _mm_srl_pi16
#define _m_psrlwi _mm_srli_pi16
#define _m_psrld _mm_srl_pi32
#define _m_psrldi _mm_srli_pi32
#define _m_psrlq _mm_srl_si64
#define _m_psrlqi _mm_srli_si64
#define _m_pand _mm_and_si64
#define _m_pandn _mm_andnot_si64
#define _m_por _mm_or_si64
#define _m_pxor _mm_xor_si64
#define _m_pcmpeqb _mm_cmpeq_pi8
#define _m_pcmpeqw _mm_cmpeq_pi16
#define _m_pcmpeqd _mm_cmpeq_pi32
#define _m_pcmpgtb _mm_cmpgt_pi8
#define _m_pcmpgtw _mm_cmpgt_pi16
#define _m_pcmpgtd _mm_cmpgt_pi32
#endif
