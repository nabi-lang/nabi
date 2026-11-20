/* vadefs.h: nabic's, over MSVC's. */
#ifndef __clang_vadefs_h
#define __clang_vadefs_h
#include_next <vadefs.h>
#ifdef _crt_va_start
#undef _crt_va_start
#define _crt_va_start(ap, param) __builtin_va_start(ap, param)
#endif
#ifdef _crt_va_end
#undef _crt_va_end
#define _crt_va_end(ap) __builtin_va_end(ap)
#endif
#ifdef _crt_va_arg
#undef _crt_va_arg
#define _crt_va_arg(ap, type) __builtin_va_arg(ap, type)
#endif
#endif
