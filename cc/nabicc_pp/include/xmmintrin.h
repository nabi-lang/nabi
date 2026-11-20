/*===---- xmmintrin.h - SSE intrinsics -------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* xmmintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __XMMINTRIN_H
#define __XMMINTRIN_H
#if !defined(__i386__) && !defined(__x86_64__)
#error "This header is only meant to be used on x86 and x64 architecture"
#endif
#include <mmintrin.h>
typedef int __v4si __attribute__((__vector_size__(16)));
typedef float __v4sf __attribute__((__vector_size__(16)));
typedef float __m128 __attribute__((__vector_size__(16), __aligned__(16)));
typedef float __m128_u __attribute__((__vector_size__(16), __aligned__(1)));
typedef unsigned int __v4su __attribute__((__vector_size__(16)));
#if __STDC_HOSTED__
#include <mm_malloc.h>
#endif
__m128 _mm_add_ss(__m128 __a, __m128 __b);
__m128 _mm_add_ps(__m128 __a, __m128 __b);
__m128 _mm_sub_ss(__m128 __a, __m128 __b);
__m128 _mm_sub_ps(__m128 __a, __m128 __b);
__m128 _mm_mul_ss(__m128 __a, __m128 __b);
__m128 _mm_mul_ps(__m128 __a, __m128 __b);
__m128 _mm_div_ss(__m128 __a, __m128 __b);
__m128 _mm_div_ps(__m128 __a, __m128 __b);
__m128 _mm_sqrt_ss(__m128 __a);
__m128 _mm_sqrt_ps(__m128 __a);
__m128 _mm_rcp_ss(__m128 __a);
__m128 _mm_rcp_ps(__m128 __a);
__m128 _mm_rsqrt_ss(__m128 __a);
__m128 _mm_rsqrt_ps(__m128 __a);
__m128 _mm_min_ss(__m128 __a, __m128 __b);
__m128 _mm_min_ps(__m128 __a, __m128 __b);
__m128 _mm_max_ss(__m128 __a, __m128 __b);
__m128 _mm_max_ps(__m128 __a, __m128 __b);
__m128 _mm_and_ps(__m128 __a, __m128 __b);
__m128 _mm_andnot_ps(__m128 __a, __m128 __b);
__m128 _mm_or_ps(__m128 __a, __m128 __b);
__m128 _mm_xor_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpeq_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpeq_ps(__m128 __a, __m128 __b);
__m128 _mm_cmplt_ss(__m128 __a, __m128 __b);
__m128 _mm_cmplt_ps(__m128 __a, __m128 __b);
__m128 _mm_cmple_ss(__m128 __a, __m128 __b);
__m128 _mm_cmple_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpgt_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpgt_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpge_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpge_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpneq_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpneq_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpnlt_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpnlt_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpnle_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpnle_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpngt_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpngt_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpnge_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpnge_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpord_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpord_ps(__m128 __a, __m128 __b);
__m128 _mm_cmpunord_ss(__m128 __a, __m128 __b);
__m128 _mm_cmpunord_ps(__m128 __a, __m128 __b);
int _mm_comieq_ss(__m128 __a, __m128 __b);
int _mm_comilt_ss(__m128 __a, __m128 __b);
int _mm_comile_ss(__m128 __a, __m128 __b);
int _mm_comigt_ss(__m128 __a, __m128 __b);
int _mm_comige_ss(__m128 __a, __m128 __b);
int _mm_comineq_ss(__m128 __a, __m128 __b);
int _mm_ucomieq_ss(__m128 __a, __m128 __b);
int _mm_ucomilt_ss(__m128 __a, __m128 __b);
int _mm_ucomile_ss(__m128 __a, __m128 __b);
int _mm_ucomigt_ss(__m128 __a, __m128 __b);
int _mm_ucomige_ss(__m128 __a, __m128 __b);
int _mm_ucomineq_ss(__m128 __a, __m128 __b);
int _mm_cvtss_si32(__m128 __a);
int _mm_cvt_ss2si(__m128 __a);
#ifdef __x86_64__
long long _mm_cvtss_si64(__m128 __a);
#endif
__m64 _mm_cvtps_pi32(__m128 __a);
__m64 _mm_cvt_ps2pi(__m128 __a);
int _mm_cvttss_si32(__m128 __a);
int _mm_cvtt_ss2si(__m128 __a);
#ifdef __x86_64__
long long _mm_cvttss_si64(__m128 __a);
#endif
__m64 _mm_cvttps_pi32(__m128 __a);
__m64 _mm_cvtt_ps2pi(__m128 __a);
__m128 _mm_cvtsi32_ss(__m128 __a, int __b);
__m128 _mm_cvt_si2ss(__m128 __a, int __b);
#ifdef __x86_64__
__m128 _mm_cvtsi64_ss(__m128 __a, long long __b);
#endif
__m128 _mm_cvtpi32_ps(__m128 __a, __m64 __b);
__m128 _mm_cvt_pi2ps(__m128 __a, __m64 __b);
float _mm_cvtss_f32(__m128 __a);
__m128 _mm_loadh_pi(__m128 __a, const __m64 *__p);
__m128 _mm_loadl_pi(__m128 __a, const __m64 *__p);
__m128 _mm_load_ss(const float *__p);
__m128 _mm_load1_ps(const float *__p);
__m128 _mm_load_ps(const float *__p);
__m128 _mm_loadu_ps(const float *__p);
__m128 _mm_loadr_ps(const float *__p);
__m128 _mm_undefined_ps(void);
__m128 _mm_set_ss(float __w);
__m128 _mm_set1_ps(float __w);
__m128 _mm_set_ps1(float __w);
__m128 _mm_set_ps(float __z, float __y, float __x, float __w);
__m128 _mm_setr_ps(float __z, float __y, float __x, float __w);
__m128 _mm_setzero_ps(void);
void _mm_storeh_pi(__m64 *__p, __m128 __a);
void _mm_storel_pi(__m64 *__p, __m128 __a);
void _mm_store_ss(float *__p, __m128 __a);
void _mm_storeu_ps(float *__p, __m128 __a);
void _mm_store_ps(float *__p, __m128 __a);
void _mm_store1_ps(float *__p, __m128 __a);
void _mm_store_ps1(float *__p, __m128 __a);
void _mm_storer_ps(float *__p, __m128 __a);
#define _MM_HINT_ET0 7
#define _MM_HINT_ET1 6
#define _MM_HINT_T0  3
#define _MM_HINT_T1  2
#define _MM_HINT_T2  1
#define _MM_HINT_NTA 0
void _mm_stream_pi(void *__p, __m64 __a);
void _mm_stream_ps(void *__p, __m128 __a);
void _mm_sfence(void);
__m64 _mm_max_pi16(__m64 __a, __m64 __b);
__m64 _mm_max_pu8(__m64 __a, __m64 __b);
__m64 _mm_min_pi16(__m64 __a, __m64 __b);
__m64 _mm_min_pu8(__m64 __a, __m64 __b);
int _mm_movemask_pi8(__m64 __a);
__m64 _mm_mulhi_pu16(__m64 __a, __m64 __b);
void _mm_maskmove_si64(__m64 __d, __m64 __n, char *__p);
__m64 _mm_avg_pu8(__m64 __a, __m64 __b);
__m64 _mm_avg_pu16(__m64 __a, __m64 __b);
__m64 _mm_sad_pu8(__m64 __a, __m64 __b);
unsigned int _mm_getcsr(void);
void _mm_setcsr(unsigned int __i);
__m128 _mm_unpackhi_ps(__m128 __a, __m128 __b);
__m128 _mm_unpacklo_ps(__m128 __a, __m128 __b);
__m128 _mm_move_ss(__m128 __a, __m128 __b);
__m128 _mm_movehl_ps(__m128 __a, __m128 __b);
__m128 _mm_movelh_ps(__m128 __a, __m128 __b);
__m128 _mm_cvtpi16_ps(__m64 __a);
__m128 _mm_cvtpu16_ps(__m64 __a);
__m128 _mm_cvtpi8_ps(__m64 __a);
__m128 _mm_cvtpu8_ps(__m64 __a);
__m128 _mm_cvtpi32x2_ps(__m64 __a, __m64 __b);
__m64 _mm_cvtps_pi16(__m128 __a);
__m64 _mm_cvtps_pi8(__m128 __a);
int _mm_movemask_ps(__m128 __a);
#define _CMP_EQ_OQ    0x00
#define _CMP_LT_OS    0x01
#define _CMP_LE_OS    0x02
#define _CMP_UNORD_Q  0x03
#define _CMP_NEQ_UQ   0x04
#define _CMP_NLT_US   0x05
#define _CMP_NLE_US   0x06
#define _CMP_ORD_Q    0x07
#define _MM_ALIGN16 __attribute__((aligned(16)))
#define _MM_SHUFFLE(z, y, x, w) (((z) << 6) | ((y) << 4) | ((x) << 2) | (w))
#define _MM_EXCEPT_INVALID    (0x0001U)
#define _MM_EXCEPT_DENORM     (0x0002U)
#define _MM_EXCEPT_DIV_ZERO   (0x0004U)
#define _MM_EXCEPT_OVERFLOW   (0x0008U)
#define _MM_EXCEPT_UNDERFLOW  (0x0010U)
#define _MM_EXCEPT_INEXACT    (0x0020U)
#define _MM_EXCEPT_MASK       (0x003fU)
#define _MM_MASK_INVALID      (0x0080U)
#define _MM_MASK_DENORM       (0x0100U)
#define _MM_MASK_DIV_ZERO     (0x0200U)
#define _MM_MASK_OVERFLOW     (0x0400U)
#define _MM_MASK_UNDERFLOW    (0x0800U)
#define _MM_MASK_INEXACT      (0x1000U)
#define _MM_MASK_MASK         (0x1f80U)
#define _MM_ROUND_NEAREST     (0x0000U)
#define _MM_ROUND_DOWN        (0x2000U)
#define _MM_ROUND_UP          (0x4000U)
#define _MM_ROUND_TOWARD_ZERO (0x6000U)
#define _MM_ROUND_MASK        (0x6000U)
#define _MM_FLUSH_ZERO_MASK   (0x8000U)
#define _MM_FLUSH_ZERO_ON     (0x8000U)
#define _MM_FLUSH_ZERO_OFF    (0x0000U)
#define _MM_GET_EXCEPTION_MASK() (_mm_getcsr() & _MM_MASK_MASK)
#define _MM_GET_EXCEPTION_STATE() (_mm_getcsr() & _MM_EXCEPT_MASK)
#define _MM_GET_FLUSH_ZERO_MODE() (_mm_getcsr() & _MM_FLUSH_ZERO_MASK)
#define _MM_GET_ROUNDING_MODE() (_mm_getcsr() & _MM_ROUND_MASK)
#define _MM_SET_EXCEPTION_MASK(x) (_mm_setcsr((_mm_getcsr() & ~_MM_MASK_MASK) | (x)))
#define _MM_SET_EXCEPTION_STATE(x) (_mm_setcsr((_mm_getcsr() & ~_MM_EXCEPT_MASK) | (x)))
#define _MM_SET_FLUSH_ZERO_MODE(x) (_mm_setcsr((_mm_getcsr() & ~_MM_FLUSH_ZERO_MASK) | (x)))
#define _MM_SET_ROUNDING_MODE(x) (_mm_setcsr((_mm_getcsr() & ~_MM_ROUND_MASK) | (x)))
#define _MM_TRANSPOSE4_PS(row0, row1, row2, row3)  do {    __m128 tmp3, tmp2, tmp1, tmp0;    tmp0 = _mm_unpacklo_ps((row0), (row1));    tmp2 = _mm_unpacklo_ps((row2), (row3));    tmp1 = _mm_unpackhi_ps((row0), (row1));    tmp3 = _mm_unpackhi_ps((row2), (row3));    (row0) = _mm_movelh_ps(tmp0, tmp2);    (row1) = _mm_movehl_ps(tmp2, tmp0);    (row2) = _mm_movelh_ps(tmp1, tmp3);    (row3) = _mm_movehl_ps(tmp3, tmp1);  } while (0)
#define _m_pextrw _mm_extract_pi16
#define _m_pinsrw _mm_insert_pi16
#define _m_pmaxsw _mm_max_pi16
#define _m_pmaxub _mm_max_pu8
#define _m_pminsw _mm_min_pi16
#define _m_pminub _mm_min_pu8
#define _m_pmovmskb _mm_movemask_pi8
#define _m_pmulhuw _mm_mulhi_pu16
#define _m_pshufw _mm_shuffle_pi16
#define _m_maskmovq _mm_maskmove_si64
#define _m_pavgb _mm_avg_pu8
#define _m_pavgw _mm_avg_pu16
#define _m_psadbw _mm_sad_pu8
#define _m_ _mm_
#if defined(__SSE2__) && !__building_module(_Builtin_intrinsics)
#include <emmintrin.h>
#endif
#endif
