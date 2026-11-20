/*===---- emmintrin.h - SSE2 intrinsics ------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* emmintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __EMMINTRIN_H
#define __EMMINTRIN_H
#if !defined(__i386__) && !defined(__x86_64__)
#error "This header is only meant to be used on x86 and x64 architecture"
#endif
#include <xmmintrin.h>
typedef double __m128d __attribute__((__vector_size__(16), __aligned__(16)));
typedef long long __m128i __attribute__((__vector_size__(16), __aligned__(16)));
typedef double __m128d_u __attribute__((__vector_size__(16), __aligned__(1)));
typedef long long __m128i_u __attribute__((__vector_size__(16), __aligned__(1)));
typedef double __v2df __attribute__((__vector_size__(16)));
typedef long long __v2di __attribute__((__vector_size__(16)));
typedef short __v8hi __attribute__((__vector_size__(16)));
typedef char __v16qi __attribute__((__vector_size__(16)));
typedef unsigned long long __v2du __attribute__((__vector_size__(16)));
typedef unsigned short __v8hu __attribute__((__vector_size__(16)));
typedef unsigned char __v16qu __attribute__((__vector_size__(16)));
typedef signed char __v16qs __attribute__((__vector_size__(16)));
#ifdef __SSE2__
typedef _Float16 __v8hf __attribute__((__vector_size__(16), __aligned__(16)));
typedef _Float16 __m128h __attribute__((__vector_size__(16), __aligned__(16)));
typedef _Float16 __m128h_u __attribute__((__vector_size__(16), __aligned__(1)));
typedef __bf16 __v8bf __attribute__((__vector_size__(16), __aligned__(16)));
typedef __bf16 __m128bh __attribute__((__vector_size__(16), __aligned__(16)));
#endif
__m128d _mm_add_sd(__m128d __a, __m128d __b);
__m128d _mm_add_pd(__m128d __a, __m128d __b);
__m128d _mm_sub_sd(__m128d __a, __m128d __b);
__m128d _mm_sub_pd(__m128d __a, __m128d __b);
__m128d _mm_mul_sd(__m128d __a, __m128d __b);
__m128d _mm_mul_pd(__m128d __a, __m128d __b);
__m128d _mm_div_sd(__m128d __a, __m128d __b);
__m128d _mm_div_pd(__m128d __a, __m128d __b);
__m128d _mm_sqrt_sd(__m128d __a, __m128d __b);
__m128d _mm_sqrt_pd(__m128d __a);
__m128d _mm_min_sd(__m128d __a, __m128d __b);
__m128d _mm_min_pd(__m128d __a, __m128d __b);
__m128d _mm_max_sd(__m128d __a, __m128d __b);
__m128d _mm_max_pd(__m128d __a, __m128d __b);
__m128d _mm_and_pd(__m128d __a, __m128d __b);
__m128d _mm_andnot_pd(__m128d __a, __m128d __b);
__m128d _mm_or_pd(__m128d __a, __m128d __b);
__m128d _mm_xor_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpeq_pd(__m128d __a, __m128d __b);
__m128d _mm_cmplt_pd(__m128d __a, __m128d __b);
__m128d _mm_cmple_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpgt_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpge_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpord_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpunord_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpneq_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpnlt_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpnle_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpngt_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpnge_pd(__m128d __a, __m128d __b);
__m128d _mm_cmpeq_sd(__m128d __a, __m128d __b);
__m128d _mm_cmplt_sd(__m128d __a, __m128d __b);
__m128d _mm_cmple_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpgt_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpge_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpord_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpunord_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpneq_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpnlt_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpnle_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpngt_sd(__m128d __a, __m128d __b);
__m128d _mm_cmpnge_sd(__m128d __a, __m128d __b);
int _mm_comieq_sd(__m128d __a, __m128d __b);
int _mm_comilt_sd(__m128d __a, __m128d __b);
int _mm_comile_sd(__m128d __a, __m128d __b);
int _mm_comigt_sd(__m128d __a, __m128d __b);
int _mm_comige_sd(__m128d __a, __m128d __b);
int _mm_comineq_sd(__m128d __a, __m128d __b);
int _mm_ucomieq_sd(__m128d __a, __m128d __b);
int _mm_ucomilt_sd(__m128d __a, __m128d __b);
int _mm_ucomile_sd(__m128d __a, __m128d __b);
int _mm_ucomigt_sd(__m128d __a, __m128d __b);
int _mm_ucomige_sd(__m128d __a, __m128d __b);
int _mm_ucomineq_sd(__m128d __a, __m128d __b);
__m128 _mm_cvtpd_ps(__m128d __a);
__m128d _mm_cvtps_pd(__m128 __a);
__m128d _mm_cvtepi32_pd(__m128i __a);
__m128i _mm_cvtpd_epi32(__m128d __a);
int _mm_cvtsd_si32(__m128d __a);
__m128 _mm_cvtsd_ss(__m128 __a, __m128d __b);
__m128d _mm_cvtsi32_sd(__m128d __a, int __b);
__m128d _mm_cvtss_sd(__m128d __a, __m128 __b);
__m128i _mm_cvttpd_epi32(__m128d __a);
int _mm_cvttsd_si32(__m128d __a);
__m64 _mm_cvtpd_pi32(__m128d __a);
__m64 _mm_cvttpd_pi32(__m128d __a);
__m128d _mm_cvtpi32_pd(__m64 __a);
double _mm_cvtsd_f64(__m128d __a);
__m128d _mm_load_pd(double const *__dp);
__m128d _mm_load1_pd(double const *__dp);
__m128d _mm_loadr_pd(double const *__dp);
__m128d _mm_loadu_pd(double const *__dp);
__m128i _mm_loadu_si64(void const *__a);
__m128i _mm_loadu_si32(void const *__a);
__m128i _mm_loadu_si16(void const *__a);
__m128d _mm_load_sd(double const *__dp);
__m128d _mm_loadh_pd(__m128d __a, double const *__dp);
__m128d _mm_loadl_pd(__m128d __a, double const *__dp);
__m128d _mm_undefined_pd(void);
__m128d _mm_set_sd(double __w);
__m128d _mm_set1_pd(double __w);
__m128d _mm_set_pd1(double __w);
__m128d _mm_set_pd(double __w, double __x);
__m128d _mm_setr_pd(double __w, double __x);
__m128d _mm_setzero_pd(void);
__m128d _mm_move_sd(__m128d __a, __m128d __b);
void _mm_store_sd(double *__dp, __m128d __a);
void _mm_store_pd(double *__dp, __m128d __a);
void _mm_store1_pd(double *__dp, __m128d __a);
void _mm_store_pd1(double *__dp, __m128d __a);
void _mm_storeu_pd(double *__dp, __m128d __a);
void _mm_storer_pd(double *__dp, __m128d __a);
void _mm_storeh_pd(double *__dp, __m128d __a);
void _mm_storel_pd(double *__dp, __m128d __a);
__m128i _mm_add_epi8(__m128i __a, __m128i __b);
__m128i _mm_add_epi16(__m128i __a, __m128i __b);
__m128i _mm_add_epi32(__m128i __a, __m128i __b);
__m64 _mm_add_si64(__m64 __a, __m64 __b);
__m128i _mm_add_epi64(__m128i __a, __m128i __b);
__m128i _mm_adds_epi8(__m128i __a, __m128i __b);
__m128i _mm_adds_epi16(__m128i __a, __m128i __b);
__m128i _mm_adds_epu8(__m128i __a, __m128i __b);
__m128i _mm_adds_epu16(__m128i __a, __m128i __b);
__m128i _mm_avg_epu8(__m128i __a, __m128i __b);
__m128i _mm_avg_epu16(__m128i __a, __m128i __b);
__m128i _mm_madd_epi16(__m128i __a, __m128i __b);
__m128i _mm_max_epi16(__m128i __a, __m128i __b);
__m128i _mm_max_epu8(__m128i __a, __m128i __b);
__m128i _mm_min_epi16(__m128i __a, __m128i __b);
__m128i _mm_min_epu8(__m128i __a, __m128i __b);
__m128i _mm_mulhi_epi16(__m128i __a, __m128i __b);
__m128i _mm_mulhi_epu16(__m128i __a, __m128i __b);
__m128i _mm_mullo_epi16(__m128i __a, __m128i __b);
__m64 _mm_mul_su32(__m64 __a, __m64 __b);
__m128i _mm_mul_epu32(__m128i __a, __m128i __b);
__m128i _mm_sad_epu8(__m128i __a, __m128i __b);
__m128i _mm_sub_epi8(__m128i __a, __m128i __b);
__m128i _mm_sub_epi16(__m128i __a, __m128i __b);
__m128i _mm_sub_epi32(__m128i __a, __m128i __b);
__m64 _mm_sub_si64(__m64 __a, __m64 __b);
__m128i _mm_sub_epi64(__m128i __a, __m128i __b);
__m128i _mm_subs_epi8(__m128i __a, __m128i __b);
__m128i _mm_subs_epi16(__m128i __a, __m128i __b);
__m128i _mm_subs_epu8(__m128i __a, __m128i __b);
__m128i _mm_subs_epu16(__m128i __a, __m128i __b);
__m128i _mm_and_si128(__m128i __a, __m128i __b);
__m128i _mm_andnot_si128(__m128i __a, __m128i __b);
__m128i _mm_or_si128(__m128i __a, __m128i __b);
__m128i _mm_xor_si128(__m128i __a, __m128i __b);
__m128i _mm_slli_epi16(__m128i __a, int __count);
__m128i _mm_sll_epi16(__m128i __a, __m128i __count);
__m128i _mm_slli_epi32(__m128i __a, int __count);
__m128i _mm_sll_epi32(__m128i __a, __m128i __count);
__m128i _mm_slli_epi64(__m128i __a, int __count);
__m128i _mm_sll_epi64(__m128i __a, __m128i __count);
__m128i _mm_srai_epi16(__m128i __a, int __count);
__m128i _mm_sra_epi16(__m128i __a, __m128i __count);
__m128i _mm_srai_epi32(__m128i __a, int __count);
__m128i _mm_sra_epi32(__m128i __a, __m128i __count);
__m128i _mm_srli_epi16(__m128i __a, int __count);
__m128i _mm_srl_epi16(__m128i __a, __m128i __count);
__m128i _mm_srli_epi32(__m128i __a, int __count);
__m128i _mm_srl_epi32(__m128i __a, __m128i __count);
__m128i _mm_srli_epi64(__m128i __a, int __count);
__m128i _mm_srl_epi64(__m128i __a, __m128i __count);
__m128i _mm_cmpeq_epi8(__m128i __a, __m128i __b);
__m128i _mm_cmpeq_epi16(__m128i __a, __m128i __b);
__m128i _mm_cmpeq_epi32(__m128i __a, __m128i __b);
__m128i _mm_cmpgt_epi8(__m128i __a, __m128i __b);
__m128i _mm_cmpgt_epi16(__m128i __a, __m128i __b);
__m128i _mm_cmpgt_epi32(__m128i __a, __m128i __b);
__m128i _mm_cmplt_epi8(__m128i __a, __m128i __b);
__m128i _mm_cmplt_epi16(__m128i __a, __m128i __b);
__m128i _mm_cmplt_epi32(__m128i __a, __m128i __b);
#ifdef __x86_64__
__m128d _mm_cvtsi64_sd(__m128d __a, long long __b);
long long _mm_cvtsd_si64(__m128d __a);
long long _mm_cvttsd_si64(__m128d __a);
#endif
__m128 _mm_cvtepi32_ps(__m128i __a);
__m128i _mm_cvtps_epi32(__m128 __a);
__m128i _mm_cvttps_epi32(__m128 __a);
__m128i _mm_cvtsi32_si128(int __a);
__m128i _mm_cvtsi64_si128(long long __a);
int _mm_cvtsi128_si32(__m128i __a);
long long _mm_cvtsi128_si64(__m128i __a);
__m128i _mm_load_si128(__m128i const *__p);
__m128i _mm_loadu_si128(__m128i_u const *__p);
__m128i _mm_loadl_epi64(__m128i_u const *__p);
__m128i _mm_undefined_si128(void);
__m128i _mm_set_epi64x(long long __q1, long long __q0);
__m128i _mm_set_epi64(__m64 __q1, __m64 __q0);
__m128i _mm_set_epi32(int __i3, int __i2, int __i1, int __i0);
__m128i _mm_set_epi16(short __w7, short __w6, short __w5, short __w4, short __w3, short __w2, short __w1, short __w0);
__m128i _mm_set_epi8(char __b15, char __b14, char __b13, char __b12, char __b11, char __b10, char __b9, char __b8, char __b7, char __b6, char __b5, char __b4, char __b3, char __b2, char __b1, char __b0);
__m128i _mm_set1_epi64x(long long __q);
__m128i _mm_set1_epi64(__m64 __q);
__m128i _mm_set1_epi32(int __i);
__m128i _mm_set1_epi16(short __w);
__m128i _mm_set1_epi8(char __b);
__m128i _mm_setr_epi64(__m64 __q0, __m64 __q1);
__m128i _mm_setr_epi32(int __i0, int __i1, int __i2, int __i3);
__m128i _mm_setr_epi16(short __w0, short __w1, short __w2, short __w3, short __w4, short __w5, short __w6, short __w7);
__m128i _mm_setr_epi8(char __b0, char __b1, char __b2, char __b3, char __b4, char __b5, char __b6, char __b7, char __b8, char __b9, char __b10, char __b11, char __b12, char __b13, char __b14, char __b15);
__m128i _mm_setzero_si128(void);
void _mm_store_si128(__m128i *__p, __m128i __b);
void _mm_storeu_si128(__m128i_u *__p, __m128i __b);
void _mm_storeu_si64(void *__p, __m128i __b);
void _mm_storeu_si32(void *__p, __m128i __b);
void _mm_storeu_si16(void *__p, __m128i __b);
void _mm_maskmoveu_si128(__m128i __d, __m128i __n, char *__p);
void _mm_storel_epi64(__m128i_u *__p, __m128i __a);
void _mm_stream_pd(void *__p, __m128d __a);
void _mm_stream_si128(void *__p, __m128i __a);
void _mm_stream_si32(void *__p, int __a);
#ifdef __x86_64__
void _mm_stream_si64(void *__p, long long __a);
#endif
void _mm_clflush(void const *__p);
void _mm_lfence(void);
void _mm_mfence(void);
__m128i _mm_packs_epi16(__m128i __a, __m128i __b);
__m128i _mm_packs_epi32(__m128i __a, __m128i __b);
__m128i _mm_packus_epi16(__m128i __a, __m128i __b);
int _mm_movemask_epi8(__m128i __a);
__m128i _mm_unpackhi_epi8(__m128i __a, __m128i __b);
__m128i _mm_unpackhi_epi16(__m128i __a, __m128i __b);
__m128i _mm_unpackhi_epi32(__m128i __a, __m128i __b);
__m128i _mm_unpackhi_epi64(__m128i __a, __m128i __b);
__m128i _mm_unpacklo_epi8(__m128i __a, __m128i __b);
__m128i _mm_unpacklo_epi16(__m128i __a, __m128i __b);
__m128i _mm_unpacklo_epi32(__m128i __a, __m128i __b);
__m128i _mm_unpacklo_epi64(__m128i __a, __m128i __b);
__m64 _mm_movepi64_pi64(__m128i __a);
__m128i _mm_movpi64_epi64(__m64 __a);
__m128i _mm_move_epi64(__m128i __a);
__m128d _mm_unpackhi_pd(__m128d __a, __m128d __b);
__m128d _mm_unpacklo_pd(__m128d __a, __m128d __b);
int _mm_movemask_pd(__m128d __a);
__m128 _mm_castpd_ps(__m128d __a);
__m128i _mm_castpd_si128(__m128d __a);
__m128d _mm_castps_pd(__m128 __a);
__m128i _mm_castps_si128(__m128 __a);
__m128 _mm_castsi128_ps(__m128i __a);
__m128d _mm_castsi128_pd(__m128i __a);
void _mm_pause(void);
#define _MM_SHUFFLE2(x, y) (((x) << 1) | (y))
#define _MM_DENORMALS_ZERO_ON (0x0040U)
#define _MM_DENORMALS_ZERO_OFF (0x0000U)
#define _MM_DENORMALS_ZERO_MASK (0x0040U)
#define _MM_GET_DENORMALS_ZERO_MODE() (_mm_getcsr() & _MM_DENORMALS_ZERO_MASK)
#define _MM_SET_DENORMALS_ZERO_MODE(x)                                            (_mm_setcsr((_mm_getcsr() & ~_MM_DENORMALS_ZERO_MASK) | (x)))
#endif
