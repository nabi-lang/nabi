/*===---- avx2intrin.h - AVX2 intrinsics -----------------------------------===
 *
 * Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
 * See https://llvm.org/LICENSE.txt for license information.
 * SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
 *
 *===-----------------------------------------------------------------------===
 */
/* avx2intrin.h: nabic's. Clang's, reduced to declarations by intrin.py:
 * the x86 intrinsics as prototypes, their types and constants; bindgen binds
 * none of them. */
#ifndef __IMMINTRIN_H
#error "Never use <avx2intrin.h> directly; include <immintrin.h> instead."
#endif
#ifndef __AVX2INTRIN_H
#define __AVX2INTRIN_H
__m256i _mm256_abs_epi8(__m256i __a);
__m256i _mm256_abs_epi16(__m256i __a);
__m256i _mm256_abs_epi32(__m256i __a);
__m256i _mm256_packs_epi16(__m256i __a, __m256i __b);
__m256i _mm256_packs_epi32(__m256i __a, __m256i __b);
__m256i _mm256_packus_epi16(__m256i __a, __m256i __b);
__m256i _mm256_packus_epi32(__m256i __V1, __m256i __V2);
__m256i _mm256_add_epi8(__m256i __a, __m256i __b);
__m256i _mm256_add_epi16(__m256i __a, __m256i __b);
__m256i _mm256_add_epi32(__m256i __a, __m256i __b);
__m256i _mm256_add_epi64(__m256i __a, __m256i __b);
__m256i _mm256_adds_epi8(__m256i __a, __m256i __b);
__m256i _mm256_adds_epi16(__m256i __a, __m256i __b);
__m256i _mm256_adds_epu8(__m256i __a, __m256i __b);
__m256i _mm256_adds_epu16(__m256i __a, __m256i __b);
__m256i _mm256_and_si256(__m256i __a, __m256i __b);
__m256i _mm256_andnot_si256(__m256i __a, __m256i __b);
__m256i _mm256_avg_epu8(__m256i __a, __m256i __b);
__m256i _mm256_avg_epu16(__m256i __a, __m256i __b);
__m256i _mm256_blendv_epi8(__m256i __V1, __m256i __V2, __m256i __M);
__m256i _mm256_cmpeq_epi8(__m256i __a, __m256i __b);
__m256i _mm256_cmpeq_epi16(__m256i __a, __m256i __b);
__m256i _mm256_cmpeq_epi32(__m256i __a, __m256i __b);
__m256i _mm256_cmpeq_epi64(__m256i __a, __m256i __b);
__m256i _mm256_cmpgt_epi8(__m256i __a, __m256i __b);
__m256i _mm256_cmpgt_epi16(__m256i __a, __m256i __b);
__m256i _mm256_cmpgt_epi32(__m256i __a, __m256i __b);
__m256i _mm256_cmpgt_epi64(__m256i __a, __m256i __b);
__m256i _mm256_hadd_epi16(__m256i __a, __m256i __b);
__m256i _mm256_hadd_epi32(__m256i __a, __m256i __b);
__m256i _mm256_hadds_epi16(__m256i __a, __m256i __b);
__m256i _mm256_hsub_epi16(__m256i __a, __m256i __b);
__m256i _mm256_hsub_epi32(__m256i __a, __m256i __b);
__m256i _mm256_hsubs_epi16(__m256i __a, __m256i __b);
__m256i _mm256_maddubs_epi16(__m256i __a, __m256i __b);
__m256i _mm256_madd_epi16(__m256i __a, __m256i __b);
__m256i _mm256_max_epi8(__m256i __a, __m256i __b);
__m256i _mm256_max_epi16(__m256i __a, __m256i __b);
__m256i _mm256_max_epi32(__m256i __a, __m256i __b);
__m256i _mm256_max_epu8(__m256i __a, __m256i __b);
__m256i _mm256_max_epu16(__m256i __a, __m256i __b);
__m256i _mm256_max_epu32(__m256i __a, __m256i __b);
__m256i _mm256_min_epi8(__m256i __a, __m256i __b);
__m256i _mm256_min_epi16(__m256i __a, __m256i __b);
__m256i _mm256_min_epi32(__m256i __a, __m256i __b);
__m256i _mm256_min_epu8(__m256i __a, __m256i __b);
__m256i _mm256_min_epu16(__m256i __a, __m256i __b);
__m256i _mm256_min_epu32(__m256i __a, __m256i __b);
int _mm256_movemask_epi8(__m256i __a);
__m256i _mm256_cvtepi8_epi16(__m128i __V);
__m256i _mm256_cvtepi8_epi32(__m128i __V);
__m256i _mm256_cvtepi8_epi64(__m128i __V);
__m256i _mm256_cvtepi16_epi32(__m128i __V);
__m256i _mm256_cvtepi16_epi64(__m128i __V);
__m256i _mm256_cvtepi32_epi64(__m128i __V);
__m256i _mm256_cvtepu8_epi16(__m128i __V);
__m256i _mm256_cvtepu8_epi32(__m128i __V);
__m256i _mm256_cvtepu8_epi64(__m128i __V);
__m256i _mm256_cvtepu16_epi32(__m128i __V);
__m256i _mm256_cvtepu16_epi64(__m128i __V);
__m256i _mm256_cvtepu32_epi64(__m128i __V);
__m256i _mm256_mul_epi32(__m256i __a, __m256i __b);
__m256i _mm256_mulhrs_epi16(__m256i __a, __m256i __b);
__m256i _mm256_mulhi_epu16(__m256i __a, __m256i __b);
__m256i _mm256_mulhi_epi16(__m256i __a, __m256i __b);
__m256i _mm256_mullo_epi16(__m256i __a, __m256i __b);
__m256i _mm256_mullo_epi32 (__m256i __a, __m256i __b);
__m256i _mm256_mul_epu32(__m256i __a, __m256i __b);
__m256i _mm256_or_si256(__m256i __a, __m256i __b);
__m256i _mm256_sad_epu8(__m256i __a, __m256i __b);
__m256i _mm256_shuffle_epi8(__m256i __a, __m256i __b);
__m256i _mm256_sign_epi8(__m256i __a, __m256i __b);
__m256i _mm256_sign_epi16(__m256i __a, __m256i __b);
__m256i _mm256_sign_epi32(__m256i __a, __m256i __b);
__m256i _mm256_slli_epi16(__m256i __a, int __count);
__m256i _mm256_sll_epi16(__m256i __a, __m128i __count);
__m256i _mm256_slli_epi32(__m256i __a, int __count);
__m256i _mm256_sll_epi32(__m256i __a, __m128i __count);
__m256i _mm256_slli_epi64(__m256i __a, int __count);
__m256i _mm256_sll_epi64(__m256i __a, __m128i __count);
__m256i _mm256_srai_epi16(__m256i __a, int __count);
__m256i _mm256_sra_epi16(__m256i __a, __m128i __count);
__m256i _mm256_srai_epi32(__m256i __a, int __count);
__m256i _mm256_sra_epi32(__m256i __a, __m128i __count);
__m256i _mm256_srli_epi16(__m256i __a, int __count);
__m256i _mm256_srl_epi16(__m256i __a, __m128i __count);
__m256i _mm256_srli_epi32(__m256i __a, int __count);
__m256i _mm256_srl_epi32(__m256i __a, __m128i __count);
__m256i _mm256_srli_epi64(__m256i __a, int __count);
__m256i _mm256_srl_epi64(__m256i __a, __m128i __count);
__m256i _mm256_sub_epi8(__m256i __a, __m256i __b);
__m256i _mm256_sub_epi16(__m256i __a, __m256i __b);
__m256i _mm256_sub_epi32(__m256i __a, __m256i __b);
__m256i _mm256_sub_epi64(__m256i __a, __m256i __b);
__m256i _mm256_subs_epi8(__m256i __a, __m256i __b);
__m256i _mm256_subs_epi16(__m256i __a, __m256i __b);
__m256i _mm256_subs_epu8(__m256i __a, __m256i __b);
__m256i _mm256_subs_epu16(__m256i __a, __m256i __b);
__m256i _mm256_unpackhi_epi8(__m256i __a, __m256i __b);
__m256i _mm256_unpackhi_epi16(__m256i __a, __m256i __b);
__m256i _mm256_unpackhi_epi32(__m256i __a, __m256i __b);
__m256i _mm256_unpackhi_epi64(__m256i __a, __m256i __b);
__m256i _mm256_unpacklo_epi8(__m256i __a, __m256i __b);
__m256i _mm256_unpacklo_epi16(__m256i __a, __m256i __b);
__m256i _mm256_unpacklo_epi32(__m256i __a, __m256i __b);
__m256i _mm256_unpacklo_epi64(__m256i __a, __m256i __b);
__m256i _mm256_xor_si256(__m256i __a, __m256i __b);
__m256i _mm256_stream_load_si256(const void *__V);
__m128 _mm_broadcastss_ps(__m128 __X);
__m128d _mm_broadcastsd_pd(__m128d __a);
__m256 _mm256_broadcastss_ps(__m128 __X);
__m256d _mm256_broadcastsd_pd(__m128d __X);
__m256i _mm256_broadcastsi128_si256(__m128i __X);
__m256i _mm256_broadcastb_epi8(__m128i __X);
__m256i _mm256_broadcastw_epi16(__m128i __X);
__m256i _mm256_broadcastd_epi32(__m128i __X);
__m256i _mm256_broadcastq_epi64(__m128i __X);
__m128i _mm_broadcastb_epi8(__m128i __X);
__m128i _mm_broadcastw_epi16(__m128i __X);
__m128i _mm_broadcastd_epi32(__m128i __X);
__m128i _mm_broadcastq_epi64(__m128i __X);
__m256i _mm256_permutevar8x32_epi32(__m256i __a, __m256i __b);
__m256 _mm256_permutevar8x32_ps(__m256 __a, __m256i __b);
__m256i _mm256_maskload_epi32(int const *__X, __m256i __M);
__m256i _mm256_maskload_epi64(long long const *__X, __m256i __M);
__m128i _mm_maskload_epi32(int const *__X, __m128i __M);
__m128i _mm_maskload_epi64(long long const *__X, __m128i __M);
void _mm256_maskstore_epi32(int *__X, __m256i __M, __m256i __Y);
void _mm256_maskstore_epi64(long long *__X, __m256i __M, __m256i __Y);
void _mm_maskstore_epi32(int *__X, __m128i __M, __m128i __Y);
void _mm_maskstore_epi64(long long *__X, __m128i __M, __m128i __Y);
__m256i _mm256_sllv_epi32(__m256i __X, __m256i __Y);
__m128i _mm_sllv_epi32(__m128i __X, __m128i __Y);
__m256i _mm256_sllv_epi64(__m256i __X, __m256i __Y);
__m128i _mm_sllv_epi64(__m128i __X, __m128i __Y);
__m256i _mm256_srav_epi32(__m256i __X, __m256i __Y);
__m128i _mm_srav_epi32(__m128i __X, __m128i __Y);
__m256i _mm256_srlv_epi32(__m256i __X, __m256i __Y);
__m128i _mm_srlv_epi32(__m128i __X, __m128i __Y);
__m256i _mm256_srlv_epi64(__m256i __X, __m256i __Y);
__m128i _mm_srlv_epi64(__m128i __X, __m128i __Y);
#endif
