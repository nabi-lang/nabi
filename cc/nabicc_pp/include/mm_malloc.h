/* mm_malloc.h: nabic's. Clang's declares its allocator over the C library's
 * (<stdlib.h>, <malloc.h> on Windows); this one only declares it. */
#ifndef __MM_MALLOC_H
#define __MM_MALLOC_H
#if !(defined(_WIN32) && defined(_mm_malloc))
void *_mm_malloc(__SIZE_TYPE__ __size, __SIZE_TYPE__ __align);
void _mm_free(void *__p);
#endif
#endif
