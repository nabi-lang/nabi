/* stdarg.h: nabic's. */

#if !defined(__need___va_list) && !defined(__need_va_list) &&                  \
    !defined(__need_va_arg) && !defined(__need___va_copy) &&                   \
    !defined(__need_va_copy)
#define __need___va_list
#define __need_va_list
#define __need_va_arg
#define __need___va_copy
#define __need_va_copy
#endif

#ifdef __need___va_list
#ifndef __GNUC_VA_LIST
#define __GNUC_VA_LIST
typedef __builtin_va_list __gnuc_va_list;
#endif
#undef __need___va_list
#endif

#ifdef __need_va_list
#ifndef _VA_LIST
typedef __builtin_va_list va_list;
#define _VA_LIST
#endif
#undef __need_va_list
#endif

#ifdef __need_va_arg
#define va_start(ap, param) __builtin_va_start(ap, param)
#define va_end(ap) __builtin_va_end(ap)
#define va_arg(ap, type) __builtin_va_arg(ap, type)
#undef __need_va_arg
#endif

#ifdef __need___va_copy
#define __va_copy(d, s) __builtin_va_copy(d, s)
#undef __need___va_copy
#endif

#ifdef __need_va_copy
#define va_copy(dest, src) __builtin_va_copy(dest, src)
#undef __need_va_copy
#endif
