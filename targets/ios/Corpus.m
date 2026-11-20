// The corpus as XCTest cases, all in the host app's process: iOS lets an
// app neither fork nor spawn, so each case program is a dylib (cc.sh) in
// the app's Frameworks/, loaded and entered at `main` on a thread of its
// own. Each dylib carries its own runtime, so its statics start fresh;
// what the process holds once for every case is put back after each:
//
//  - shim.c sends exit, _exit, abort, atexit, mmap, munmap,
//    pthread_create and _tlv_atexit here. An exit on the case's thread
//    runs the case's atexit handlers and jumps back out, as one in work
//    the case queued on the main thread does; on another thread it ends
//    the case and parks that thread. A fatal signal (a fault, a trap, a
//    raised abort) does the same, with 128 + the signal as the exit code.
//  - Threads the case started and that outlive it (an async runtime's
//    workers, a detached thread) are made to end: each one blocked in a
//    wait is signalled (RECLAIM) and returns from its start routine. One
//    that keeps computing is left to run; it ends nothing more (`stale`).
//  - The runtime's TLS is a thread-local variable, and dyld takes a
//    pthread key for each image that has one, at its first use. A
//    process has 512 keys and dyld never gives an image's back, so after
//    some 450 cases every runtime aborted in its first TLS access. Once
//    nothing of a case can run again (its threads all gone, no thread
//    nabi did not start adopted by it), its key is deleted.
//  - The heap reservations the runtime asks for are capped (CAP), and
//    unmapped on the same condition: an iPhone's address space is not a
//    Mac's.
//  - The environment, the working directory, argv, the signal actions
//    and stdin / stdout / stderr are the case's while it runs and are put
//    back after. The host's own log lines (os_log mirrored to stderr under
//    Xcode) are not the case's stderr.
//
// stdin is /dev/null and stdout / stderr go to files while the case
// runs; the checks are integration.rs's (exit code, stderr patterns,
// stdout). Each gc-stress run loads a copy of the dylib (<name>~<i>), a
// runtime of its own. The case list (Cases.inc, one test method per case)
// and the expectations (cases/<name>.json in this bundle) come from
// xctest.sh; the cases in skip.txt are skipped.

#import <XCTest/XCTest.h>
#import <crt_externs.h>
#import <dlfcn.h>
#import <errno.h>
#import <execinfo.h>
#import <fcntl.h>
#import <mach-o/dyld.h>
#import <mach-o/getsect.h>
#import <pthread.h>
#import <setjmp.h>
#import <signal.h>
#import <sys/mman.h>
#import <sys/syscall.h>
#import <stdlib.h>
#import <string.h>
#import <unistd.h>

struct nabi_corpus_hooks {
	void (*exit)(int code) __attribute__((noreturn));
	void (*quit)(int code) __attribute__((noreturn));
	int (*atexit)(void (*fn)(void));
	void *(*mmap)(void *addr, size_t len, int prot, int flags, int fd, off_t off);
	int (*munmap)(void *addr, size_t len);
	int (*spawn)(pthread_t *t, const pthread_attr_t *a, void *(*fn)(void *), void *arg);
	void (*adopted)(void);
};

// A reservation the case's runtime may hold; it halves its ask until one
// fits (nabir_gc's RESERVE). thread_gc_malloc_growth needs more than
// 256 MiB.
#define CAP ((size_t)1 << 30)
// Reservations at least this large are the case's to release.
#define TRACKED ((size_t)16 << 20)
#define MAX_REGIONS 64
#define MAX_ATEXIT 64
#define LIMIT 60.0
// How long the threads a case left get to end.
#define RECLAIM_LIMIT 2.0
// Ends a finished case's threads; nothing in an app uses it.
#define RECLAIM SIGUSR2

static const int fatal[] = {SIGSEGV, SIGBUS, SIGILL, SIGTRAP, SIGFPE, SIGABRT, SIGSYS};

// A thread a case started (hook_spawn): its case, and where RECLAIM
// sends it.
struct spawned {
	void *(*fn)(void *);
	void *arg;
	_Atomic int *live;
	unsigned gen;
	pthread_t self;
	sigjmp_buf home;
	volatile int ready; // `home` is set
	struct spawned *next;
	struct spawned **prev;
};

// The case running now; written by the harness between cases, by the
// case's threads while it runs (first end wins).
static struct {
	volatile int active;
	volatile unsigned gen; // one per run, never 0
	pthread_t thread;
	sigjmp_buf jump;
	volatile int posted; // ended on another thread
	volatile int ended;
	volatile int code;
	volatile int own; // ended on its own thread
	volatile int adopted; // the runtime adopted a thread it did not start
	void (*atexit[MAX_ATEXIT])(void);
	int natexit;
	pthread_mutex_t lock;
	struct {
		void *addr;
		size_t len;
	} regions[MAX_REGIONS];
	int nregions;
	// Threads the case started and that still run; one counter per case,
	// as a finished case's threads may end any time.
	_Atomic int *live;
	struct spawned *threads; // every case's, under `lock`
	int (*main)(int, char **, char **, char **);
	char *argv[2];
	char trace[1024]; // where an abort's backtrace goes
} cur = {.lock = PTHREAD_MUTEX_INITIALIZER};

static pthread_key_t spawned_key;
// Runs, those whose heap and TLS key stay (a thread left or adopted),
// and the threads RECLAIM ended.
static int runs, kept;
static _Atomic int reclaimed;

static void run_atexit(void)
{
	while (cur.natexit > 0)
		cur.atexit[--cur.natexit]();
}

// The first end of the case: its code. Whether this call was first.
static int finish(int code)
{
	pthread_mutex_lock(&cur.lock);
	int first = !cur.ended;
	if (first) {
		cur.ended = 1;
		cur.code = code;
	}
	pthread_mutex_unlock(&cur.lock);
	return first;
}

static void __attribute__((noreturn)) park(void)
{
	sigset_t none;
	sigemptyset(&none);
	for (;;)
		sigsuspend(&none);
}

// A thread of a case that has ended.
static int stale_thread(void)
{
	struct spawned *s = pthread_getspecific(spawned_key);
	return s && s->gen != cur.gen;
}

// The harness's main thread, waiting for a case and running what the
// case queued on it: an exit there jumps back to the wait (wait_case).
static sigjmp_buf main_jump;
static volatile int main_armed;

static void __attribute__((noreturn)) leave(int code, int handlers)
{
	if (stale_thread())
		park();
	if (cur.active && main_armed && pthread_main_np()) {
		if (handlers)
			run_atexit();
		finish(code);
		siglongjmp(main_jump, 1);
	}
	if (cur.active && pthread_equal(pthread_self(), cur.thread)) {
		if (handlers)
			run_atexit();
		finish(code);
		siglongjmp(cur.jump, 1);
	}
	if (handlers)
		run_atexit();
	if (finish(code))
		cur.posted = 1;
	park();
}

static void __attribute__((noreturn)) hook_exit(int code)
{
	leave(code, 1);
}

static void backtrace_to(const char *path, const char *head)
{
	int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
	if (fd < 0)
		return;
	void *pcs[64];
	write(fd, head, strlen(head));
	backtrace_symbols_fd(pcs, backtrace(pcs, 64), fd);
	close(fd);
}

// An abort says where it came from (beside the case's output, not in it).
static void __attribute__((noreturn)) hook_quit(int code)
{
	if (code == 134 && cur.active && !stale_thread())
		backtrace_to(cur.trace, "abort\n");
	leave(code, 0);
}

static int hook_atexit(void (*fn)(void))
{
	if (cur.natexit == MAX_ATEXIT)
		return -1;
	cur.atexit[cur.natexit++] = fn;
	return 0;
}

static void *hook_mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off)
{
	if (prot == PROT_NONE && len > CAP + CAP / 2) {
		errno = ENOMEM;
		return MAP_FAILED;
	}
	void *p = mmap(addr, len, prot, flags, fd, off);
	if (p != MAP_FAILED && prot == PROT_NONE && len >= TRACKED) {
		pthread_mutex_lock(&cur.lock);
		if (cur.nregions < MAX_REGIONS) {
			cur.regions[cur.nregions].addr = p;
			cur.regions[cur.nregions].len = len;
			cur.nregions++;
		}
		pthread_mutex_unlock(&cur.lock);
	}
	return p;
}

static int hook_munmap(void *addr, size_t len)
{
	pthread_mutex_lock(&cur.lock);
	for (int i = 0; i < cur.nregions; i++) {
		if (cur.regions[i].addr == addr) {
			cur.regions[i] = cur.regions[--cur.nregions];
			break;
		}
	}
	pthread_mutex_unlock(&cur.lock);
	return munmap(addr, len);
}

static void spawned_gone(void *p)
{
	struct spawned *s = p;
	pthread_mutex_lock(&cur.lock);
	if (s->next)
		s->next->prev = s->prev;
	*s->prev = s->next;
	pthread_mutex_unlock(&cur.lock);
	(*s->live)--;
	free(s);
}

static void *spawned_start(void *p)
{
	struct spawned *s = p;
	s->self = pthread_self();
	pthread_setspecific(spawned_key, s);
	// RECLAIM comes back here; the thread then ends.
	if (sigsetjmp(s->home, 1)) {
		reclaimed++;
		return NULL;
	}
	s->ready = 1;
	return s->fn(s->arg);
}

static int counted_spawn(_Atomic int *live, unsigned gen, pthread_t *t, const pthread_attr_t *a,
			 void *(*fn)(void *), void *arg)
{
	struct spawned *s = calloc(1, sizeof *s);
	if (!s)
		return EAGAIN;
	s->fn = fn;
	s->arg = arg;
	s->live = live;
	s->gen = gen;
	pthread_mutex_lock(&cur.lock);
	s->next = cur.threads;
	if (s->next)
		s->next->prev = &s->next;
	s->prev = &cur.threads;
	cur.threads = s;
	pthread_mutex_unlock(&cur.lock);
	(*live)++;
	int e = pthread_create(t, a, spawned_start, s);
	if (e) {
		(*live)--;
		pthread_mutex_lock(&cur.lock);
		if (s->next)
			s->next->prev = s->prev;
		*s->prev = s->next;
		pthread_mutex_unlock(&cur.lock);
		free(s);
	}
	return e;
}

// A thread of a finished case starting another counts for that case.
static int hook_spawn(pthread_t *t, const pthread_attr_t *a, void *(*fn)(void *), void *arg)
{
	struct spawned *s = pthread_getspecific(spawned_key);
	if (s && s->gen != cur.gen)
		return counted_spawn(s->live, s->gen, t, a, fn, arg);
	return counted_spawn(cur.live, cur.gen, t, a, fn, arg);
}

static void hook_adopted(void)
{
	cur.adopted = 1;
}

static struct nabi_corpus_hooks hooks = {
	hook_exit, hook_quit, hook_atexit, hook_mmap, hook_munmap, hook_spawn, hook_adopted,
};

// A finished case's threads that still run end nothing more and release
// nothing. A case's dylib stays loaded, so they find these.
static void __attribute__((noreturn)) stale_end(int code)
{
	(void)code;
	park();
}

static int stale_atexit(void (*fn)(void))
{
	(void)fn;
	return 0;
}

static void *stale_mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off)
{
	if (prot == PROT_NONE && len > CAP + CAP / 2) {
		errno = ENOMEM;
		return MAP_FAILED;
	}
	return mmap(addr, len, prot, flags, fd, off);
}

static void stale_adopted(void)
{
}

static struct nabi_corpus_hooks stale = {
	stale_end, stale_end, stale_atexit, stale_mmap, munmap, hook_spawn, stale_adopted,
};

static void on_fatal(int sig, siginfo_t *info, void *ctx)
{
	(void)info;
	(void)ctx;
	// A finished case's thread found its heap gone, or another end.
	if (stale_thread())
		park();
	if (!cur.active) {
		signal(sig, SIG_DFL);
		return;
	}
	char head[32];
	snprintf(head, sizeof head, "signal %d\n", sig);
	backtrace_to(cur.trace, head);
	leave(128 + sig, 0);
}

// The waits a thread holds no lock of libSystem's in: one interrupted in
// any other system call (a write under a stdio lock, a free unmapping
// under malloc's) or not in one is left alone.
static int waiting(long nr)
{
	switch (nr) {
	case SYS_psynch_cvwait:
	case SYS_psynch_mutexwait:
	case SYS_psynch_rw_rdlock:
	case SYS_psynch_rw_wrlock:
	case SYS_ulock_wait:
	case SYS_ulock_wait2:
	case SYS___semwait_signal:
	case SYS___semwait_signal_nocancel:
	case SYS_kevent:
	case SYS_kevent_qos:
	case SYS_kevent_id:
	case SYS_select:
	case SYS_select_nocancel:
	case SYS_poll:
	case SYS_poll_nocancel:
	case SYS_sigsuspend:
	case SYS___sigwait:
		return 1;
	}
	return 0;
}

// RECLAIM on a finished case's thread: one blocked in a wait goes back
// to its start routine and ends.
static void on_reclaim(int sig, siginfo_t *info, void *ctx)
{
	(void)sig;
	(void)info;
	struct spawned *s = pthread_getspecific(spawned_key);
	if (!s || !s->ready || s->gen == cur.gen)
		return;
	const ucontext_t *uc = ctx;
	const uint32_t *pc =
		(const uint32_t *)__darwin_arm_thread_state64_get_pc(uc->uc_mcontext->__ss);
	// The kernel returns past the `svc #0x80`, or to it to restart.
	const uint32_t svc = 0xd4001001;
	if (pc[-1] != svc && pc[0] != svc)
		return;
	if (!waiting((long)uc->uc_mcontext->__ss.__x[16]))
		return;
	siglongjmp(s->home, 1);
}

static void catch_signals(void)
{
	struct sigaction sa = {0};
	sa.sa_sigaction = on_fatal;
	sa.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_NODEFER;
	sigemptyset(&sa.sa_mask);
	for (size_t i = 0; i < sizeof fatal / sizeof *fatal; i++)
		sigaction(fatal[i], &sa, NULL);
	struct sigaction rc = {0};
	rc.sa_sigaction = on_reclaim;
	rc.sa_flags = SA_SIGINFO;
	sigemptyset(&rc.sa_mask);
	sigaction(RECLAIM, &rc, NULL);
}

// Ends what is left of the case `gen`: its threads blocked in a wait,
// within RECLAIM_LIMIT. Whether none is left.
static BOOL reclaim(unsigned gen, _Atomic int *live)
{
	catch_signals();
	CFAbsoluteTime deadline = CFAbsoluteTimeGetCurrent() + RECLAIM_LIMIT;
	while (*live > 0 && CFAbsoluteTimeGetCurrent() < deadline) {
		pthread_mutex_lock(&cur.lock);
		for (struct spawned *s = cur.threads; s; s = s->next)
			if (s->gen == gen && s->ready)
				pthread_kill(s->self, RECLAIM);
		pthread_mutex_unlock(&cur.lock);
		usleep(2000);
	}
	return *live == 0;
}

// The pthread keys dyld took for the image's thread-local variables
// (the runtime has one). A key not taken yet reads 0, which dyld never
// hands out to an image.
static int tlv_keys(const void *in, pthread_key_t *keys, int max)
{
	Dl_info info;
	if (!dladdr(in, &info))
		return 0;
	unsigned long size = 0;
	const struct {
		void *thunk;
		unsigned long key;
		unsigned long offset;
	} *d = (const void *)getsectiondata(info.dli_fbase, "__DATA", "__thread_vars", &size);
	int n = 0;
	for (unsigned long i = 0; d && i < size / sizeof *d; i++) {
		if (!d[i].key)
			continue;
		int seen = 0;
		for (int j = 0; j < n; j++)
			seen |= keys[j] == (pthread_key_t)d[i].key;
		if (!seen && n < max)
			keys[n++] = (pthread_key_t)d[i].key;
	}
	return n;
}

static void *case_thread(void *arg)
{
	(void)arg;
	cur.thread = pthread_self();
	if (sigsetjmp(cur.jump, 1) == 0) {
		char *envp[] = {NULL};
		char *apple[] = {NULL};
		int code = cur.main(1, cur.argv, envp, apple);
		run_atexit();
		finish(code);
	}
	cur.own = 1;
	return NULL;
}

// Wait for the case's end, at most `limit` seconds, keeping the main
// thread's run loop going: a case may queue work on it. Whether the case
// ended on its own thread, which is then joined.
static BOOL wait_case(pthread_t t, double limit)
{
	CFAbsoluteTime deadline = CFAbsoluteTimeGetCurrent() + limit;
	main_armed = 1;
	if (sigsetjmp(main_jump, 1)) {
		// Ended by an exit in work it queued here; its thread may still
		// run, so it is not joined.
		main_armed = 0;
		return NO;
	}
	for (;;) {
		if (cur.ended && cur.own) {
			main_armed = 0;
			pthread_join(t, NULL);
			return YES;
		}
		if (cur.posted || CFAbsoluteTimeGetCurrent() > deadline)
			break;
		CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.002, true);
	}
	main_armed = 0;
	return NO;
}

// Rust's str::lines: split on \n, a trailing \r dropped, no last empty line.
static NSArray<NSString *> *lines(NSString *s)
{
	NSMutableArray *out = [NSMutableArray array];
	if (s.length == 0)
		return out;
	if ([s hasSuffix:@"\n"])
		s = [s substringToIndex:s.length - 1];
	for (NSString *l in [s componentsSeparatedByString:@"\n"])
		[out addObject:[l hasSuffix:@"\r"] ? [l substringToIndex:l.length - 1] : l];
	return out;
}

static NSString *trim_end(NSString *s)
{
	NSCharacterSet *ws = NSCharacterSet.whitespaceAndNewlineCharacterSet;
	NSUInteger n = s.length;
	while (n > 0 && [ws characterIsMember:[s characterAtIndex:n - 1]])
		n--;
	return [s substringToIndex:n];
}

// `*` stands for any text, as integration.rs's glob.
static BOOL glob(NSString *pat, NSString *line)
{
	// hasPrefix: and hasSuffix: are false for an empty string.
	NSArray<NSString *> *parts = [pat componentsSeparatedByString:@"*"];
	if (parts[0].length && ![line hasPrefix:parts[0]])
		return NO;
	NSString *rest = [line substringFromIndex:parts[0].length];
	if (parts.count == 1)
		return rest.length == 0;
	for (NSUInteger i = 1; i + 1 < parts.count; i++) {
		NSRange r = [rest rangeOfString:parts[i]];
		if (r.location == NSNotFound)
			return NO;
		rest = [rest substringFromIndex:r.location + r.length];
	}
	return parts.lastObject.length == 0 || [rest hasSuffix:parts.lastObject];
}

// What `environ` holds, as name -> value.
static NSDictionary *environment(void)
{
	NSMutableDictionary *d = [NSMutableDictionary dictionary];
	for (char **e = *_NSGetEnviron(); *e; e++) {
		const char *eq = strchr(*e, '=');
		if (!eq)
			continue;
		NSString *k = [[NSString alloc] initWithBytes:*e
						       length:eq - *e
						     encoding:NSUTF8StringEncoding];
		NSString *v = @(eq + 1);
		if (k && v)
			d[k] = v;
	}
	return d;
}

// Back to `want`: what the case set or removed, undone.
static void set_environment(NSDictionary *want)
{
	NSDictionary *now = environment();
	for (NSString *k in now)
		if (!want[k])
			unsetenv(k.UTF8String);
	for (NSString *k in want)
		if (![now[k] isEqualToString:want[k]])
			setenv(k.UTF8String, [want[k] UTF8String], 1);
}

// A line the host process logged (`2026-11-18 20:46:03.420820+0900
// Host[14432:101599884] ...`), not the case.
static BOOL host_log(NSString *l)
{
	static NSRegularExpression *re;
	static dispatch_once_t once;
	dispatch_once(&once, ^{
		re = [NSRegularExpression
			regularExpressionWithPattern:
				@"^\\d{4}-\\d\\d-\\d\\d \\d\\d:\\d\\d:\\d\\d\\.\\d+[+-]\\d{4} Host\\[\\d+:\\d+\\] "
					     options:0
					       error:nil];
	});
	return [re firstMatchInString:l options:0 range:NSMakeRange(0, l.length)] != nil;
}

static NSString *read_file(NSString *path)
{
	NSData *d = [NSData dataWithContentsOfFile:path];
	NSString *s = d ? [[NSString alloc] initWithData:d encoding:NSUTF8StringEncoding] : nil;
	// Not UTF-8: as Latin-1, so the mismatch still shows.
	if (!s && d)
		s = [[NSString alloc] initWithData:d encoding:NSISOLatin1StringEncoding];
	return s ?: @"";
}

static NSString *json(id v)
{
	NSData *d = [NSJSONSerialization dataWithJSONObject:v
						    options:NSJSONWritingFragmentsAllowed
						      error:nil];
	return d ? [[NSString alloc] initWithData:d encoding:NSUTF8StringEncoding] : @"?";
}

@interface NabiCorpus : XCTestCase
@end

@implementation NabiCorpus

+ (void)setUp
{
	[super setUp];
	pthread_key_create(&spawned_key, spawned_gone);
	cur.gen = 1;
}

+ (void)tearDown
{
	NSLog(@"corpus: %d runs, %d kept their heap and TLS key, %d threads reclaimed", runs, kept,
	      (int)reclaimed);
	[super tearDown];
}

static NSDictionary<NSString *, NSString *> *skips(void)
{
	static NSDictionary *m;
	static dispatch_once_t once;
	dispatch_once(&once, ^{
		NSMutableDictionary *d = [NSMutableDictionary dictionary];
		NSBundle *b = [NSBundle bundleForClass:NabiCorpus.class];
		NSString *text = read_file([b pathForResource:@"skip" ofType:@"txt"]);
		for (NSString *raw in [text componentsSeparatedByString:@"\n"]) {
			NSString *l = [raw stringByTrimmingCharactersInSet:
						   NSCharacterSet.whitespaceCharacterSet];
			if (l.length == 0 || [l hasPrefix:@"#"])
				continue;
			NSRange sp = [l rangeOfString:@" "];
			if (sp.location == NSNotFound)
				d[l] = @"listed in skip.txt";
			else
				d[[l substringToIndex:sp.location]] =
					[l substringFromIndex:sp.location + 1];
		}
		m = d;
	});
	return m;
}

// nil when the run matched; else what differed.
- (NSString *)runOnce:(NSDictionary *)c dylib:(NSString *)dylib env:(NSDictionary *)env
{
	NSFileManager *fm = NSFileManager.defaultManager;
	NSString *dir = [NSTemporaryDirectory()
		stringByAppendingPathComponent:[@"corpus" stringByAppendingPathComponent:c[@"name"]]];
	[fm removeItemAtPath:dir error:nil];
	NSString *tmp = [dir stringByAppendingPathComponent:@"t"];
	NSString *cwd = [dir stringByAppendingPathComponent:@"w"];
	[fm createDirectoryAtPath:tmp withIntermediateDirectories:YES attributes:nil error:nil];
	[fm createDirectoryAtPath:cwd withIntermediateDirectories:YES attributes:nil error:nil];
	NSString *outp = [dir stringByAppendingPathComponent:@"stdout"];
	NSString *errp = [dir stringByAppendingPathComponent:@"stderr"];
	NSString *tracep = [dir stringByAppendingPathComponent:@"abort"];
	strlcpy(cur.trace, tracep.fileSystemRepresentation, sizeof cur.trace);

	void *h = dlopen(dylib.fileSystemRepresentation, RTLD_NOW | RTLD_LOCAL);
	if (!h)
		return [NSString stringWithFormat:@"dlopen: %s", dlerror()];
	struct nabi_corpus_hooks **hp = dlsym(h, "nabi_corpus_hooks");
	cur.main = (int (*)(int, char **, char **, char **))dlsym(h, "main");
	if (!hp || !cur.main)
		return @"the dylib lacks main or nabi_corpus_hooks (not linked by cc.sh)";

	// The process's state the case may see or change, saved.
	NSDictionary *old = environment();
	catch_signals();
	struct sigaction actions[NSIG];
	for (int i = 1; i < NSIG; i++)
		sigaction(i, NULL, &actions[i]);
	NSMutableDictionary *set = [NSMutableDictionary dictionaryWithDictionary:env];
	for (NSString *v in @[ @"TMPDIR", @"TMP", @"TEMP" ])
		set[v] = tmp;
	for (NSString *k in set)
		setenv(k.UTF8String, [set[k] UTF8String], 1);
	int here = open(".", O_RDONLY | O_DIRECTORY);
	chdir(cwd.fileSystemRepresentation);
	int argc = *_NSGetArgc();
	char **argv = *_NSGetArgv();
	cur.argv[0] = (char *)dylib.fileSystemRepresentation;
	cur.argv[1] = NULL;
	*_NSGetArgc() = 1;
	*_NSGetArgv() = cur.argv;

	fflush(stdout);
	fflush(stderr);
	int saved[3] = {dup(0), dup(1), dup(2)};
	int in = open("/dev/null", O_RDONLY);
	int out = open(outp.fileSystemRepresentation, O_WRONLY | O_CREAT | O_TRUNC, 0644);
	int err = open(errp.fileSystemRepresentation, O_WRONLY | O_CREAT | O_TRUNC, 0644);
	dup2(in, 0);
	dup2(out, 1);
	dup2(err, 2);
	close(in);
	close(out);
	close(err);

	cur.live = calloc(1, sizeof *cur.live);
	cur.posted = 0;
	cur.ended = 0;
	cur.own = 0;
	cur.code = -1;
	cur.adopted = 0;
	cur.natexit = 0;
	cur.nregions = 0;
	*hp = &hooks;
	cur.active = 1;

	pthread_attr_t attr;
	pthread_attr_init(&attr);
	// A main thread's stack, as the cases expect, and its priority: the
	// main thread waits on it.
	pthread_attr_setstacksize(&attr, 8 << 20);
	pthread_attr_set_qos_class_np(&attr, QOS_CLASS_USER_INTERACTIVE, 0);
	pthread_t t;
	int e = counted_spawn(cur.live, cur.gen, &t, &attr, case_thread, NULL);
	pthread_attr_destroy(&attr);
	if (e) {
		cur.active = 0;
		*hp = &stale;
		finish(-1);
	}
	double limit = MIN([c[@"timeout"] doubleValue], LIMIT);
	BOOL joined = e ? YES : wait_case(t, limit);
	BOOL ended = cur.ended;
	int code = cur.code;
	cur.active = 0;
	*hp = &stale;
	// From here the case's threads are stale_thread()s.
	unsigned gen = cur.gen;
	cur.gen = gen + 1 ? gen + 1 : 1;
	BOOL gone = reclaim(gen, cur.live);
	if (!e && !joined)
		pthread_detach(t);

	fflush(stdout);
	fflush(stderr);
	for (int i = 0; i < 3; i++) {
		dup2(saved[i], i);
		close(saved[i]);
	}
	*_NSGetArgc() = argc;
	*_NSGetArgv() = argv;
	fchdir(here);
	close(here);
	set_environment(old);
	for (int i = 1; i < NSIG; i++)
		if (i != SIGKILL && i != SIGSTOP)
			sigaction(i, &actions[i], NULL);
	// Nothing of the case runs again: its heap and its TLS key go.
	runs++;
	if (!gone || cur.adopted)
		kept++;
	if (gone && !cur.adopted) {
		for (int i = 0; i < cur.nregions; i++)
			munmap(cur.regions[i].addr, cur.regions[i].len);
		cur.nregions = 0;
		pthread_key_t keys[4];
		int n = tlv_keys((const void *)cur.main, keys, 4);
		for (int i = 0; i < n; i++) {
			pthread_setspecific(keys[i], NULL);
			pthread_key_delete(keys[i]);
		}
	}

	if (!ended)
		return [NSString stringWithFormat:@"timed out after %.0fs", limit];
	NSString *stderr_ = read_file(errp);
	NSString *stdout_ = read_file(outp);
	NSString *trace = read_file(tracep);
	[fm removeItemAtPath:dir error:nil];
	int want = [c[@"exit"] intValue];
	if (code != want) {
		NSArray *l = lines(stderr_);
		NSUInteger from = l.count > 20 ? l.count - 20 : 0;
		return [NSString stringWithFormat:@"exit=%d (expected %d)\n    stderr: %@%@%@", code, want,
			[[l subarrayWithRange:NSMakeRange(from, l.count - from)]
				componentsJoinedByString:@"\n      "],
			trace.length ? @"\n    abort at:\n" : @"", trace];
	}
	NSString *norm = [stderr_ stringByReplacingOccurrencesOfString:@"\\" withString:@"/"];
	NSString *root = c[@"root"];
	if (root.length)
		norm = [norm stringByReplacingOccurrencesOfString:root withString:@""];
	NSMutableArray *got = [NSMutableArray array];
	for (NSString *l in lines(norm))
		if (!host_log(l))
			[got addObject:[l stringByTrimmingCharactersInSet:
					  NSCharacterSet.whitespaceAndNewlineCharacterSet]];
	NSArray *pats = c[@"stderr"];
	BOOL ok = got.count == pats.count;
	for (NSUInteger i = 0; ok && i < got.count; i++)
		ok = glob(pats[i], got[i]);
	if (!ok)
		return [NSString stringWithFormat:@"stderr mismatch\n  want: %@\n  got:  %@",
			json(pats), json(got)];
	if (![trim_end(stdout_) isEqualToString:trim_end(c[@"stdout"])])
		return [NSString stringWithFormat:@"stdout mismatch\n  want: %@\n  got:  %@",
			json(c[@"stdout"]), json(stdout_)];
	return nil;
}

- (void)runCase:(NSString *)name
{
	NSString *why = skips()[name];
	if (why)
		XCTSkip(@"%@", why);
	NSBundle *b = [NSBundle bundleForClass:NabiCorpus.class];
	NSString *path = [b pathForResource:name ofType:@"json" inDirectory:@"cases"];
	NSData *d = [NSData dataWithContentsOfFile:path];
	XCTAssertNotNil(d, @"no cases/%@.json in the test bundle", name);
	if (!d)
		return;
	NSDictionary *c = [NSJSONSerialization JSONObjectWithData:d options:0 error:nil];
	NSString *frameworks = NSBundle.mainBundle.privateFrameworksPath;
	NSArray *runs = c[@"runs"];
	NSArray *labels = @[ @"", @"gc-stress: ", @"gc-stress concurrent: " ];
	for (NSUInteger i = 0; i < runs.count; i++) {
		NSString *file = i ? [NSString stringWithFormat:@"%@~%lu.dylib", name, (unsigned long)i]
				   : [name stringByAppendingString:@".dylib"];
		NSString *e = [self runOnce:c
				      dylib:[frameworks stringByAppendingPathComponent:file]
					env:runs[i]];
		if (e) {
			XCTFail(@"%@%@", i < labels.count ? labels[i] : @"", e);
			return;
		}
	}
}

#include "Cases.inc"

@end
