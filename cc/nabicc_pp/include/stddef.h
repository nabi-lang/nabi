/* stddef.h: nabic's, from the target's predefined macros. */

#if defined(__musl__) && __has_include_next(<stddef.h>)
/* musl's own, as clang does. */
#include_next <stddef.h>
#else

#if !defined(__need_ptrdiff_t) && !defined(__need_size_t) &&                   \
    !defined(__need_rsize_t) && !defined(__need_wchar_t) &&                    \
    !defined(__need_NULL) && !defined(__need_max_align_t) &&                   \
    !defined(__need_offsetof) && !defined(__need_wint_t)
#define __need_ptrdiff_t
#define __need_size_t
#if defined(__STDC_WANT_LIB_EXT1__) && __STDC_WANT_LIB_EXT1__ >= 1
#define __need_rsize_t
#endif
#define __need_wchar_t
#if !defined(__STDDEF_H)
#define __need_NULL
#endif
#define __need_max_align_t
#define __need_offsetof
#define __STDDEF_H
#endif

#ifdef __need_ptrdiff_t
#ifndef _PTRDIFF_T
#define _PTRDIFF_T
typedef __PTRDIFF_TYPE__ ptrdiff_t;
#endif
#undef __need_ptrdiff_t
#endif

#ifdef __need_size_t
#ifndef _SIZE_T
#define _SIZE_T
typedef __SIZE_TYPE__ size_t;
#endif
#undef __need_size_t
#endif

#ifdef __need_rsize_t
#ifndef _RSIZE_T
#define _RSIZE_T
typedef __SIZE_TYPE__ rsize_t;
#endif
#undef __need_rsize_t
#endif

#ifdef __need_wchar_t
#ifndef _WCHAR_T
#define _WCHAR_T
#ifdef _MSC_EXTENSIONS
#define _WCHAR_T_DEFINED
#endif
typedef __WCHAR_TYPE__ wchar_t;
#endif
#undef __need_wchar_t
#endif

#ifdef __need_NULL
#undef NULL
#define NULL ((void*)0)
#undef __need_NULL
#endif

#ifdef __need_max_align_t
#ifndef __CLANG_MAX_ALIGN_T_DEFINED
#define __CLANG_MAX_ALIGN_T_DEFINED
#if defined(_MSC_VER)
typedef double max_align_t;
#elif defined(__APPLE__)
typedef long double max_align_t;
#else
typedef struct {
  long long __clang_max_align_nonce1
      __attribute__((__aligned__(__alignof__(long long))));
  long double __clang_max_align_nonce2
      __attribute__((__aligned__(__alignof__(long double))));
} max_align_t;
#endif
#endif
#undef __need_max_align_t
#endif

#ifdef __need_offsetof
#define offsetof(t, d) __builtin_offsetof(t, d)
#undef __need_offsetof
#endif

#ifdef __need_wint_t
#ifndef _WINT_T
#define _WINT_T
typedef __WINT_TYPE__ wint_t;
#endif
#undef __need_wint_t
#endif

#endif
