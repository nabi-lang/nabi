// Linked into each corpus case's dylib (cc.sh): what would end or outlive
// the process goes to the XCTest harness (Corpus.m) instead, through the
// hooks it sets after dlopen. Definitions here win over libSystem's for
// the case's own references, the runtime's included; pthread_create too,
// so the harness knows the case's threads, and _tlv_atexit, which the
// runtime calls only on a thread it did not start (a GCD worker, the
// main thread). Before the hooks are set (initializers at dlopen), the
// real functions run.

#include <dlfcn.h>
#include <pthread.h>
#include <stddef.h>
#include <sys/types.h>

struct nabi_corpus_hooks {
	void (*exit)(int code) __attribute__((noreturn));
	void (*quit)(int code) __attribute__((noreturn));
	int (*atexit)(void (*fn)(void));
	void *(*mmap)(void *addr, size_t len, int prot, int flags, int fd, off_t off);
	int (*munmap)(void *addr, size_t len);
	int (*spawn)(pthread_t *t, const pthread_attr_t *a, void *(*fn)(void *), void *arg);
	void (*adopted)(void);
};

struct nabi_corpus_hooks *nabi_corpus_hooks;

static void *real(const char *name)
{
	return dlsym(RTLD_NEXT, name);
}

void exit(int code)
{
	if (nabi_corpus_hooks)
		nabi_corpus_hooks->exit(code);
	((void (*)(int))real("exit"))(code);
	__builtin_unreachable();
}

void _exit(int code)
{
	if (nabi_corpus_hooks)
		nabi_corpus_hooks->quit(code);
	((void (*)(int))real("_exit"))(code);
	__builtin_unreachable();
}

void _Exit(int code)
{
	_exit(code);
}

// 128 + SIGABRT, as a shell reports the death.
void abort(void)
{
	if (nabi_corpus_hooks)
		nabi_corpus_hooks->quit(134);
	((void (*)(void))real("abort"))();
	__builtin_unreachable();
}

int atexit(void (*fn)(void))
{
	if (nabi_corpus_hooks)
		return nabi_corpus_hooks->atexit(fn);
	return ((int (*)(void (*)(void)))real("atexit"))(fn);
}

void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off)
{
	if (nabi_corpus_hooks)
		return nabi_corpus_hooks->mmap(addr, len, prot, flags, fd, off);
	return ((void *(*)(void *, size_t, int, int, int, off_t))real("mmap"))(addr, len, prot,
										 flags, fd, off);
}

int munmap(void *addr, size_t len)
{
	if (nabi_corpus_hooks)
		return nabi_corpus_hooks->munmap(addr, len);
	return ((int (*)(void *, size_t))real("munmap"))(addr, len);
}

int pthread_create(pthread_t *t, const pthread_attr_t *a, void *(*fn)(void *), void *arg)
{
	if (nabi_corpus_hooks)
		return nabi_corpus_hooks->spawn(t, a, fn, arg);
	return ((int (*)(pthread_t *, const pthread_attr_t *, void *(*)(void *), void *))real(
		"pthread_create"))(t, a, fn, arg);
}

void _tlv_atexit(void (*fn)(void *), void *arg)
{
	if (nabi_corpus_hooks)
		nabi_corpus_hooks->adopted();
	((void (*)(void (*)(void *), void *))real("_tlv_atexit"))(fn, arg);
}
