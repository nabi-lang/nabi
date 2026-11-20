/*===---- avxintrin.h - AVX intrinsics -------------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* avxintrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __IMMINTRIN_H
#error "Never use <avxintrin.h> directly; include <immintrin.h> instead."
#endif
#ifndef __AVXINTRIN_H
#define __AVXINTRIN_H
typedef double __v4df __attribute__ ((__vector_size__ (32)));
typedef float __v8sf __attribute__ ((__vector_size__ (32)));
typedef long long __v4di __attribute__ ((__vector_size__ (32)));
typedef int __v8si __attribute__ ((__vector_size__ (32)));
typedef short __v16hi __attribute__ ((__vector_size__ (32)));
typedef char __v32qi __attribute__ ((__vector_size__ (32)));
typedef unsigned long long __v4du __attribute__ ((__vector_size__ (32)));
typedef unsigned int __v8su __attribute__ ((__vector_size__ (32)));
typedef unsigned short __v16hu __attribute__ ((__vector_size__ (32)));
typedef unsigned char __v32qu __attribute__ ((__vector_size__ (32)));
typedef signed char __v32qs __attribute__((__vector_size__(32)));
typedef float __m256 __attribute__ ((__vector_size__ (32), __aligned__(32)));
typedef double __m256d __attribute__((__vector_size__(32), __aligned__(32)));
typedef long long __m256i __attribute__((__vector_size__(32), __aligned__(32)));
typedef float __m256_u __attribute__ ((__vector_size__ (32), __aligned__(1)));
typedef double __m256d_u __attribute__((__vector_size__(32), __aligned__(1)));
typedef long long __m256i_u __attribute__((__vector_size__(32), __aligned__(1)));
#ifdef __SSE2__
typedef _Float16 __v16hf __attribute__((__vector_size__(32), __aligned__(32)));
typedef _Float16 __m256h __attribute__((__vector_size__(32), __aligned__(32)));
typedef _Float16 __m256h_u __attribute__((__vector_size__(32), __aligned__(1)));
typedef __bf16 __v16bf __attribute__((__vector_size__(32), __aligned__(32)));
typedef __bf16 __m256bh __attribute__((__vector_size__(32), __aligned__(32)));
#endif
__m256d _mm256_add_pd(__m256d __a, __m256d __b);
__m256 _mm256_add_ps(__m256 __a, __m256 __b);
__m256d _mm256_sub_pd(__m256d __a, __m256d __b);
__m256 _mm256_sub_ps(__m256 __a, __m256 __b);
__m256d _mm256_addsub_pd(__m256d __a, __m256d __b);
__m256 _mm256_addsub_ps(__m256 __a, __m256 __b);
__m256d _mm256_div_pd(__m256d __a, __m256d __b);
__m256 _mm256_div_ps(__m256 __a, __m256 __b);
__m256d _mm256_max_pd(__m256d __a, __m256d __b);
__m256 _mm256_max_ps(__m256 __a, __m256 __b);
__m256d _mm256_min_pd(__m256d __a, __m256d __b);
__m256 _mm256_min_ps(__m256 __a, __m256 __b);
__m256d _mm256_mul_pd(__m256d __a, __m256d __b);
__m256 _mm256_mul_ps(__m256 __a, __m256 __b);
__m256d _mm256_sqrt_pd(__m256d __a);
__m256 _mm256_sqrt_ps(__m256 __a);
__m256 _mm256_rsqrt_ps(__m256 __a);
__m256 _mm256_rcp_ps(__m256 __a);
__m256d _mm256_and_pd(__m256d __a, __m256d __b);
__m256 _mm256_and_ps(__m256 __a, __m256 __b);
__m256d _mm256_andnot_pd(__m256d __a, __m256d __b);
__m256 _mm256_andnot_ps(__m256 __a, __m256 __b);
__m256d _mm256_or_pd(__m256d __a, __m256d __b);
__m256 _mm256_or_ps(__m256 __a, __m256 __b);
__m256d _mm256_xor_pd(__m256d __a, __m256d __b);
__m256 _mm256_xor_ps(__m256 __a, __m256 __b);
__m256d _mm256_hadd_pd(__m256d __a, __m256d __b);
__m256 _mm256_hadd_ps(__m256 __a, __m256 __b);
__m256d _mm256_hsub_pd(__m256d __a, __m256d __b);
__m256 _mm256_hsub_ps(__m256 __a, __m256 __b);
__m128d _mm_permutevar_pd(__m128d __a, __m128i __c);
__m256d _mm256_permutevar_pd(__m256d __a, __m256i __c);
__m128 _mm_permutevar_ps(__m128 __a, __m128i __c);
__m256 _mm256_permutevar_ps(__m256 __a, __m256i __c);
__m256d _mm256_blendv_pd(__m256d __a, __m256d __b, __m256d __c);
__m256 _mm256_blendv_ps(__m256 __a, __m256 __b, __m256 __c);
#define _CMP_EQ_UQ    0x08
#define _CMP_NGE_US   0x09
#define _CMP_NGT_US   0x0a
#define _CMP_FALSE_OQ 0x0b
#define _CMP_NEQ_OQ   0x0c
#define _CMP_GE_OS    0x0d
#define _CMP_GT_OS    0x0e
#define _CMP_TRUE_UQ  0x0f
#define _CMP_EQ_OS    0x10
#define _CMP_LT_OQ    0x11
#define _CMP_LE_OQ    0x12
#define _CMP_UNORD_S  0x13
#define _CMP_NEQ_US   0x14
#define _CMP_NLT_UQ   0x15
#define _CMP_NLE_UQ   0x16
#define _CMP_ORD_S    0x17
#define _CMP_EQ_US    0x18
#define _CMP_NGE_UQ   0x19
#define _CMP_NGT_UQ   0x1a
#define _CMP_FALSE_OS 0x1b
#define _CMP_NEQ_OS   0x1c
#define _CMP_GE_OQ    0x1d
#define _CMP_GT_OQ    0x1e
#define _CMP_TRUE_US  0x1f
__m256d _mm256_cvtepi32_pd(__m128i __a);
__m256 _mm256_cvtepi32_ps(__m256i __a);
__m128 _mm256_cvtpd_ps(__m256d __a);
__m256i _mm256_cvtps_epi32(__m256 __a);
__m256d _mm256_cvtps_pd(__m128 __a);
__m128i _mm256_cvttpd_epi32(__m256d __a);
__m128i _mm256_cvtpd_epi32(__m256d __a);
__m256i _mm256_cvttps_epi32(__m256 __a);
double _mm256_cvtsd_f64(__m256d __a);
int _mm256_cvtsi256_si32(__m256i __a);
float _mm256_cvtss_f32(__m256 __a);
__m256 _mm256_movehdup_ps(__m256 __a);
__m256 _mm256_moveldup_ps(__m256 __a);
__m256d _mm256_movedup_pd(__m256d __a);
__m256d _mm256_unpackhi_pd(__m256d __a, __m256d __b);
__m256d _mm256_unpacklo_pd(__m256d __a, __m256d __b);
__m256 _mm256_unpackhi_ps(__m256 __a, __m256 __b);
__m256 _mm256_unpacklo_ps(__m256 __a, __m256 __b);
int _mm_testz_pd(__m128d __a, __m128d __b);
int _mm_testc_pd(__m128d __a, __m128d __b);
int _mm_testnzc_pd(__m128d __a, __m128d __b);
int _mm_testz_ps(__m128 __a, __m128 __b);
int _mm_testc_ps(__m128 __a, __m128 __b);
int _mm_testnzc_ps(__m128 __a, __m128 __b);
int _mm256_testz_pd(__m256d __a, __m256d __b);
int _mm256_testc_pd(__m256d __a, __m256d __b);
int _mm256_testnzc_pd(__m256d __a, __m256d __b);
int _mm256_testz_ps(__m256 __a, __m256 __b);
int _mm256_testc_ps(__m256 __a, __m256 __b);
int _mm256_testnzc_ps(__m256 __a, __m256 __b);
int _mm256_testz_si256(__m256i __a, __m256i __b);
int _mm256_testc_si256(__m256i __a, __m256i __b);
int _mm256_testnzc_si256(__m256i __a, __m256i __b);
int _mm256_movemask_pd(__m256d __a);
int _mm256_movemask_ps(__m256 __a);
void _mm256_zeroall(void);
void _mm256_zeroupper(void);
__m128 _mm_broadcast_ss(float const *__a);
__m256d _mm256_broadcast_sd(double const *__a);
__m256 _mm256_broadcast_ss(float const *__a);
__m256d _mm256_broadcast_pd(__m128d const *__a);
__m256 _mm256_broadcast_ps(__m128 const *__a);
__m256d _mm256_load_pd(double const *__p);
__m256 _mm256_load_ps(float const *__p);
__m256d _mm256_loadu_pd(double const *__p);
__m256 _mm256_loadu_ps(float const *__p);
__m256i _mm256_load_si256(__m256i const *__p);
__m256i _mm256_loadu_si256(__m256i_u const *__p);
__m256i _mm256_lddqu_si256(__m256i_u const *__p);
void _mm256_store_pd(double *__p, __m256d __a);
void _mm256_store_ps(float *__p, __m256 __a);
void _mm256_storeu_pd(double *__p, __m256d __a);
void _mm256_storeu_ps(float *__p, __m256 __a);
void _mm256_store_si256(__m256i *__p, __m256i __a);
void _mm256_storeu_si256(__m256i_u *__p, __m256i __a);
__m128d _mm_maskload_pd(double const *__p, __m128i __m);
__m256d _mm256_maskload_pd(double const *__p, __m256i __m);
__m128 _mm_maskload_ps(float const *__p, __m128i __m);
__m256 _mm256_maskload_ps(float const *__p, __m256i __m);
void _mm256_maskstore_ps(float *__p, __m256i __m, __m256 __a);
void _mm_maskstore_pd(double *__p, __m128i __m, __m128d __a);
void _mm256_maskstore_pd(double *__p, __m256i __m, __m256d __a);
void _mm_maskstore_ps(float *__p, __m128i __m, __m128 __a);
void _mm256_stream_si256(void *__a, __m256i __b);
void _mm256_stream_pd(void *__a, __m256d __b);
void _mm256_stream_ps(void *__p, __m256 __a);
__m256d _mm256_undefined_pd(void);
__m256 _mm256_undefined_ps(void);
__m256i _mm256_undefined_si256(void);
__m256d _mm256_set_pd(double __a, double __b, double __c, double __d);
__m256 _mm256_set_ps(float __a, float __b, float __c, float __d, float __e, float __f, float __g, float __h);
__m256i _mm256_set_epi32(int __i0, int __i1, int __i2, int __i3, int __i4, int __i5, int __i6, int __i7);
__m256i _mm256_set_epi16(short __w15, short __w14, short __w13, short __w12, short __w11, short __w10, short __w09, short __w08, short __w07, short __w06, short __w05, short __w04, short __w03, short __w02, short __w01, short __w00);
__m256i _mm256_set_epi8(char __b31, char __b30, char __b29, char __b28, char __b27, char __b26, char __b25, char __b24, char __b23, char __b22, char __b21, char __b20, char __b19, char __b18, char __b17, char __b16, char __b15, char __b14, char __b13, char __b12, char __b11, char __b10, char __b09, char __b08, char __b07, char __b06, char __b05, char __b04, char __b03, char __b02, char __b01, char __b00);
__m256i _mm256_set_epi64x(long long __a, long long __b, long long __c, long long __d);
__m256d _mm256_setr_pd(double __a, double __b, double __c, double __d);
__m256 _mm256_setr_ps(float __a, float __b, float __c, float __d, float __e, float __f, float __g, float __h);
__m256i _mm256_setr_epi32(int __i0, int __i1, int __i2, int __i3, int __i4, int __i5, int __i6, int __i7);
__m256i _mm256_setr_epi16(short __w15, short __w14, short __w13, short __w12, short __w11, short __w10, short __w09, short __w08, short __w07, short __w06, short __w05, short __w04, short __w03, short __w02, short __w01, short __w00);
__m256i _mm256_setr_epi8(char __b31, char __b30, char __b29, char __b28, char __b27, char __b26, char __b25, char __b24, char __b23, char __b22, char __b21, char __b20, char __b19, char __b18, char __b17, char __b16, char __b15, char __b14, char __b13, char __b12, char __b11, char __b10, char __b09, char __b08, char __b07, char __b06, char __b05, char __b04, char __b03, char __b02, char __b01, char __b00);
__m256i _mm256_setr_epi64x(long long __a, long long __b, long long __c, long long __d);
__m256d _mm256_set1_pd(double __w);
__m256 _mm256_set1_ps(float __w);
__m256i _mm256_set1_epi32(int __i);
__m256i _mm256_set1_epi16(short __w);
__m256i _mm256_set1_epi8(char __b);
__m256i _mm256_set1_epi64x(long long __q);
__m256d _mm256_setzero_pd(void);
__m256 _mm256_setzero_ps(void);
__m256i _mm256_setzero_si256(void);
__m256 _mm256_castpd_ps(__m256d __a);
__m256i _mm256_castpd_si256(__m256d __a);
__m256d _mm256_castps_pd(__m256 __a);
__m256i _mm256_castps_si256(__m256 __a);
__m256 _mm256_castsi256_ps(__m256i __a);
__m256d _mm256_castsi256_pd(__m256i __a);
__m128d _mm256_castpd256_pd128(__m256d __a);
__m128 _mm256_castps256_ps128(__m256 __a);
__m128i _mm256_castsi256_si128(__m256i __a);
__m256d _mm256_castpd128_pd256(__m128d __a);
__m256 _mm256_castps128_ps256(__m128 __a);
__m256i _mm256_castsi128_si256(__m128i __a);
__m256d _mm256_zextpd128_pd256(__m128d __a);
__m256 _mm256_zextps128_ps256(__m128 __a);
__m256i _mm256_zextsi128_si256(__m128i __a);
__m256 _mm256_set_m128 (__m128 __hi, __m128 __lo);
__m256d _mm256_set_m128d (__m128d __hi, __m128d __lo);
__m256i _mm256_set_m128i (__m128i __hi, __m128i __lo);
__m256 _mm256_setr_m128 (__m128 __lo, __m128 __hi);
__m256d _mm256_setr_m128d (__m128d __lo, __m128d __hi);
__m256i _mm256_setr_m128i (__m128i __lo, __m128i __hi);
__m256 _mm256_loadu2_m128(float const *__addr_hi, float const *__addr_lo);
__m256d _mm256_loadu2_m128d(double const *__addr_hi, double const *__addr_lo);
__m256i _mm256_loadu2_m128i(__m128i_u const *__addr_hi, __m128i_u const *__addr_lo);
void _mm256_storeu2_m128(float *__addr_hi, float *__addr_lo, __m256 __a);
void _mm256_storeu2_m128d(double *__addr_hi, double *__addr_lo, __m256d __a);
void _mm256_storeu2_m128i(__m128i_u *__addr_hi, __m128i_u *__addr_lo, __m256i __a);
#endif
