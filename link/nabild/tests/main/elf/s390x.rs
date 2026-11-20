//! The ELF linker for z/Architecture: big-endian s390x C programs (lazy
//! PLT calls, TLS in every model compilers emit, data relocations,
//! debug info, libm, threads) and nabi's runtime into PIEs against
//! glibc, with lld's PLT and GOT.
//!
//! The tests need clang with the `SystemZ` target and an s390x glibc:
//! `NABI_S390X_LIBC` (a directory with `Scrt1.o`), else the cargo target
//! directory's `lib/nabi/targets/s390x-unknown-linux-gnu`
//! (`targets/build.sh`), else `/usr/s390x-linux-gnu/lib`; the runtime is
//! `NABI_S390X_RUNTIME`, else that directory's `libnabir.a`. Executables
//! run under `qemu-s390x`, when there.

use nabic_target::TargetSpec;
use std::path::{Path, PathBuf};
use std::process::Command;

const TRIPLE: &str = "s390x-unknown-linux-gnu";
const CLANG: &str = "s390x-linux-gnu";
const PREFIX: &str = "/usr/s390x-linux-gnu";

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(&format!("link-elf-s390x-{name}"))
}

const fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

/// The s390x C runtime, if there.
fn libc_dir() -> Option<PathBuf> {
    let t = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    [
        std::env::var_os("NABI_S390X_LIBC").map(PathBuf::from),
        Some(t.join("lib/nabi/targets").join(TRIPLE)),
        Some(Path::new(PREFIX).join("lib")),
    ]
    .into_iter()
    .flatten()
    .find(|d| d.join("Scrt1.o").is_file())
}

/// nabi's s390x runtime, if built.
fn runtime(libc: &Path) -> Option<PathBuf> {
    std::env::var_os("NABI_S390X_RUNTIME")
        .map(PathBuf::from)
        .or_else(|| Some(libc.join("libnabir.a")))
        .filter(|p| p.is_file())
}

/// `clang --target=s390x-linux-gnu -c` of `src` with `flags`, or `None`
/// without such a clang.
fn cc(dir: &Path, name: &str, src: &str, flags: &[&str]) -> Option<PathBuf> {
    let c = dir.join(format!("{name}.c"));
    let o = dir.join(format!("{name}.o"));
    std::fs::write(&c, src).unwrap();
    let s = Command::new("clang")
        .arg(format!("--target={CLANG}"))
        .args(["-c", "-g", "-o"])
        .arg(&o)
        .args(flags)
        .arg(&c)
        .status()
        .ok()?;
    s.success().then_some(o)
}

/// Run `exe` under qemu-user with the cross loader, when both are there.
fn run(exe: &Path) -> Option<std::process::Output> {
    if !Path::new(PREFIX).join("lib/ld64.so.1").exists() {
        return None;
    }
    Command::new("qemu-s390x")
        .arg("-L")
        .arg(PREFIX)
        .arg(exe)
        .output()
        .ok()
}

/// No libc headers: what the programs use, declared.
const DECLS: &str = r"
typedef unsigned long size_t;
typedef unsigned long pthread_t;
int printf(const char *, ...);
void *malloc(size_t);
void free(void *);
double sqrt(double);
double cos(double);
int pthread_create(pthread_t *, const void *, void *(*)(void *), void *);
int pthread_join(pthread_t, void **);
";

/// TLS of every model the compiler keeps in PIC code: general dynamic
/// and local dynamic (`__tls_get_offset`, relaxed to local exec),
/// initial exec through the GOT; GOT loads of data.
const TLS_PIC: &str = r#"
extern __thread int tgd;
static __thread int tld = 2;
static __thread int tld_bss;
extern __thread int tie __attribute__((tls_model("initial-exec")));
extern int plain;
int get_gd(void) { return tgd; }
int get_ld(void) { return tld + tld_bss; }
void bump_ld(void) { tld += 10; tld_bss += 1; }
int get_ie(void) { return tie; }
int *addr_gd(void) { return &tgd; }
int get_plain(void) { return plain; }
unsigned long divide(unsigned long a, unsigned long b) { return a / b; }
"#;

const MAIN: &str = r#"
__thread int tgd = 1;
__thread int tie = 3;
__thread int tle = 4;
__thread int tbss[16];
int plain = 5;
int get_gd(void);
int get_ld(void);
void bump_ld(void);
int get_ie(void);
int *addr_gd(void);
int get_plain(void);
unsigned long divide(unsigned long, unsigned long);
static int add(int a, int b) { return a + b; }
static int mul(int a, int b) { return a * b; }
static int (*const ops[])(int, int) = { add, mul };
static const char *names[] = { "add", "mul" };
int (*volatile fp)(int, int) = add;
double (*volatile libm)(double) = sqrt;
extern void weak_missing(void) __attribute__((weak));

static void *thread(void *arg) {
    int *r = arg;
    r[0] = get_gd() + get_ld() + get_ie() + tle + tbss[3];
    *addr_gd() = 100;
    return 0;
}

int main(void) {
    volatile double x = 2.0;
    printf("sqrt %.3f cos %.3f ptr %.1f\n", sqrt(x), cos(x - 2.0), libm(16.0));
    for (int i = 0; i < 2; i++)
        printf("%s %d\n", names[i], ops[i](6, 7));
    printf("fp %d div %lu\n", fp(1, 2), divide(100, 7));
    int *p = malloc(4 * sizeof(int));
    p[3] = 42;
    printf("malloc %d\n", p[3]);
    free(p);
    tgd += 10;
    bump_ld();
    tbss[3] = 5;
    printf("tls %d %d %d %d %d %d\n", get_gd(), get_ld(), get_ie(), tle, tbss[3], get_plain());
    int r[1];
    pthread_t t;
    pthread_create(&t, 0, thread, r);
    pthread_join(t, 0);
    printf("thread %d main %d %d\n", r[0], tgd, *addr_gd());
    printf("weak %d\n", weak_missing == 0);
    return 0;
}
"#;

const OUT: &str = "sqrt 1.414 cos 1.000 ptr 4.0\nadd 13\nmul 42\nfp 3 div 14\nmalloc 42\n\
                   tls 11 13 3 4 5 5\nthread 10 main 11 11\nweak 1\n";

/// An s390x PIE: big endian, `.got` starting with `_DYNAMIC`, PLT
/// slots in `.got.plt` pointing back into their entries until bound.
fn check(b: &[u8]) {
    assert!(b.starts_with(b"\x7fELF\x02\x02"));
    assert_eq!(u16_at(b, 16), 3, "ET_DYN");
    assert_eq!(u16_at(b, 18), 22, "EM_S390");
    let (shoff, shnum, shstrndx) = (
        u64_at(b, 40) as usize,
        u16_at(b, 60) as usize,
        u16_at(b, 62) as usize,
    );
    let sh = |i: usize| shoff + 64 * i;
    let strs = u64_at(b, sh(shstrndx) + 24) as usize;
    let name = |i: usize| {
        let at = strs + u32_at(b, sh(i)) as usize;
        let end = b[at..].iter().position(|&c| c == 0).unwrap();
        String::from_utf8_lossy(&b[at..at + end]).into_owned()
    };
    let find = |n: &str| {
        (0..shnum)
            .find(|&i| name(i) == n)
            .unwrap_or_else(|| panic!("{n}"))
    };
    let addr = |i: usize| u64_at(b, sh(i) + 16);
    let off = |i: usize| u64_at(b, sh(i) + 24) as usize;
    let (got, dynamic, plt, gotplt) = (
        find(".got"),
        find(".dynamic"),
        find(".plt"),
        find(".got.plt"),
    );
    assert_eq!(u64_at(b, off(got)), addr(dynamic), ".got[0]: _DYNAMIC");
    assert_eq!(
        u64_at(b, off(gotplt)),
        addr(plt) + 32 + 14,
        "the first slot: its entry's basr"
    );
    let rela = find(".rela.plt");
    assert_eq!(u32_at(b, off(rela) + 12), 11, "R_390_JMP_SLOT");
}

#[test]
fn c_programs_against_glibc() {
    let Some(libc) = libc_dir() else {
        eprintln!("no s390x C runtime: skipped");
        return;
    };
    let Some(rt) = runtime(&libc) else {
        eprintln!("no s390x runtime (targets/build.sh {TRIPLE}): skipped");
        return;
    };
    let dir = scratch("c");
    let t = TargetSpec::builtin(TRIPLE).unwrap();
    assert!(nabild::links(&t));
    let variants: [(&str, &[&str], &[&str]); 3] = [
        ("O0", &["-O0", "-fPIE"], &["-O0", "-fPIE"]),
        ("O2", &["-O2", "-fPIE"], &["-O2", "-fPIE"]),
        ("pic", &["-O2", "-fPIE"], &["-O2", "-fPIC"]),
    ];
    for (v, main_flags, tls_flags) in variants {
        let main = format!("{DECLS}{MAIN}");
        let Some(m) = cc(&dir, &format!("main-{v}"), &main, main_flags) else {
            eprintln!("{v}: no clang for SystemZ: skipped");
            return;
        };
        let tls = cc(&dir, &format!("tls-{v}"), TLS_PIC, tls_flags).unwrap();
        for strip in [false, true] {
            let exe = dir.join(format!("prog-{v}-{strip}"));
            // The runtime brings compiler-rt's helpers.
            let inputs = [m.as_os_str(), tls.as_os_str(), rt.as_os_str()];
            nabild::link_executable(&t, &inputs, &exe, strip, Some(&libc))
                .unwrap_or_else(|e| panic!("{v}: {e}"));
            check(&std::fs::read(&exe).unwrap());
            if let Some(o) = run(&exe) {
                assert!(o.status.success(), "{v}: {o:?}");
                assert_eq!(String::from_utf8_lossy(&o.stdout), OUT, "{v}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// nabi's runtime, whole: rustc's code (local exec TLS, GOT loads,
/// big-endian `eh_frame`), compiler-rt, and its libc imports, called
/// through its C entry points.
#[test]
fn runtime_archive() {
    let Some(libc) = libc_dir() else {
        eprintln!("no s390x C runtime: skipped");
        return;
    };
    let Some(rt) = runtime(&libc) else {
        eprintln!("no s390x runtime (targets/build.sh {TRIPLE}): skipped");
        return;
    };
    let dir = scratch("rt");
    let t = TargetSpec::builtin(TRIPLE).unwrap();
    let src = r#"
typedef unsigned long size_t;
int printf(const char *, ...);
size_t nabi_cpu_count(void);
size_t nabi_cstr_len(const char *);
unsigned char *nabi_alloc_raw(size_t, size_t, int);
int main(void) {
    unsigned char *p = nabi_alloc_raw(16, 8, 1);
    volatile unsigned __int128 a = 1000000000000ull, b = 7;
    printf("cpus %d cstr %lu zeroed %d div %lu\n", nabi_cpu_count() > 0,
           nabi_cstr_len("hello"), p[15], (unsigned long)(a / b));
    return 0;
}
"#;
    let Some(m) = cc(&dir, "main", src, &["-O2", "-fPIE"]) else {
        eprintln!("no clang for SystemZ: skipped");
        return;
    };
    for strip in [false, true] {
        let exe = dir.join(format!("rt-{strip}"));
        let inputs = [
            m.as_os_str(),
            "--whole-archive".as_ref(),
            rt.as_os_str(),
            "--no-whole-archive".as_ref(),
        ];
        nabild::link_executable(&t, &inputs, &exe, strip, Some(&libc))
            .unwrap_or_else(|e| panic!("{e}"));
        check(&std::fs::read(&exe).unwrap());
        if let Some(o) = run(&exe) {
            assert!(o.status.success(), "{o:?}");
            assert_eq!(
                String::from_utf8_lossy(&o.stdout),
                "cpus 1 cstr 5 zeroed 0 div 142857142857\n"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sequences nabi's code generator emits, by hand: `brasl`, `larl`,
/// GOT loads (`GOTENT`, of a local function and of `printf`), initial
/// exec TLS (`INDNTPOFF`) with the thread pointer from `a0:a1`.
const ASM: &str = r#"
    .section .tbss,"awT",@nobits
    .p2align 2
    .globl nt
nt: .zero 4
    .data
    .p2align 3
cell: .quad 41
    .text
    .globl main
    .type main,@function
main:
    stmg %r6, %r15, 48(%r15)
    aghi %r15, -160
    lgrl %r1, nt@INDNTPOFF
    ear %r2, %a0
    sllg %r2, %r2, 32
    ear %r2, %a1
    lghi %r3, 7
    st %r3, 0(%r1,%r2)
    larl %r4, cell
    lg %r6, 0(%r4)
    lgrl %r5, plus_one@GOTENT
    basr %r14, %r5
    brasl %r14, twice@PLT
    larl %r2, fmt
    lgr %r3, %r6
    lgrl %r1, printf@GOTENT
    basr %r14, %r1
    lghi %r2, 0
    lmg %r6, %r15, 208(%r15)
    br %r14
    .size main, .-main

    # A GOT load of a local label refers to its section: the assembler
    # would leave `.text + offset`, which no GOT slot stands for.
    .globl plus_one
    .hidden plus_one
    .type plus_one,@function
plus_one:
    aghi %r6, 1
    br %r14

    .type twice,@function
twice:
    lgrl %r1, nt@INDNTPOFF
    ear %r2, %a0
    sllg %r2, %r2, 32
    ear %r2, %a1
    lgf %r3, 0(%r1,%r2)
    agr %r6, %r6
    agr %r6, %r3
    br %r14

    .section .rodata.str1.2,"aMS",@progbits,1
    .p2align 1
fmt: .asciz "asm %ld\n"
"#;

#[test]
fn code_generator_sequences() {
    let Some(libc) = libc_dir() else {
        eprintln!("no s390x C runtime: skipped");
        return;
    };
    let dir = scratch("asm");
    let s = dir.join("seq.s");
    std::fs::write(&s, ASM).unwrap();
    let o = dir.join("seq.o");
    let ok = Command::new("clang")
        .arg(format!("--target={CLANG}"))
        .args(["-c", "-o"])
        .arg(&o)
        .arg(&s)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no clang for SystemZ: skipped");
        return;
    }
    let t = TargetSpec::builtin(TRIPLE).unwrap();
    let exe = dir.join("seq");
    nabild::link_executable(&t, &[o.as_os_str()], &exe, false, Some(&libc))
        .unwrap_or_else(|e| panic!("{e}"));
    check(&std::fs::read(&exe).unwrap());
    if let Some(r) = run(&exe) {
        assert!(r.status.success(), "{r:?}");
        // (41 + 1) * 2 + 7
        assert_eq!(String::from_utf8_lossy(&r.stdout), "asm 91\n");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
