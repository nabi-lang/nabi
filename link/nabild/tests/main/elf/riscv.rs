//! The ELF linker for RISC-V: riscv64 C programs (PLT calls, TLS in every
//! model, data relocations, debug info, libm, threads) and nabi's runtime
//! into PIEs against glibc, and a freestanding riscv32 program into a
//! static image without an OS, laid out as ld.lld lays it out.
//!
//! The riscv64 tests need clang with the RISC-V target and a riscv64
//! glibc: `NABI_RISCV64_LIBC` (a directory with `Scrt1.o`), else the
//! cargo target directory's `lib/nabi/targets/riscv64gc-unknown-linux-gnu`
//! (`targets/build.sh`), else `/usr/riscv64-linux-gnu/lib`; the runtime
//! is `NABI_RISCV64_RUNTIME`, else that directory's `libnabir.a`.
//! Executables run under `qemu-riscv64` / `qemu-riscv32`, when there;
//! `NABI_LLD` names an ld.lld to compare the riscv32 layout with.

use nabic_target::TargetSpec;
use std::path::{Path, PathBuf};
use std::process::Command;

const TRIPLE: &str = "riscv64gc-unknown-linux-gnu";
const TRIPLE32: &str = "riscv32imac-unknown-none-elf";
const PREFIX: &str = "/usr/riscv64-linux-gnu";

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(&format!("link-elf-riscv-{name}"))
}

const fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The riscv64 C runtime, if there.
fn libc_dir() -> Option<PathBuf> {
    let t = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    [
        std::env::var_os("NABI_RISCV64_LIBC").map(PathBuf::from),
        Some(t.join("lib/nabi/targets").join(TRIPLE)),
        Some(Path::new(PREFIX).join("lib")),
    ]
    .into_iter()
    .flatten()
    .find(|d| d.join("Scrt1.o").is_file())
}

/// nabi's riscv64 runtime, if built.
fn runtime(libc: &Path) -> Option<PathBuf> {
    std::env::var_os("NABI_RISCV64_RUNTIME")
        .map(PathBuf::from)
        .or_else(|| Some(libc.join("libnabir.a")))
        .filter(|p| p.is_file())
}

/// `clang --target=<target> -c` of `src` with `flags`, or `None`
/// without such a clang.
fn cc(dir: &Path, name: &str, src: &str, target: &str, flags: &[&str]) -> Option<PathBuf> {
    let c = dir.join(format!("{name}.c"));
    let o = dir.join(format!("{name}.o"));
    std::fs::write(&c, src).unwrap();
    let s = Command::new("clang")
        .arg(format!("--target={target}"))
        .args(["-c", "-g", "-o"])
        .arg(&o)
        .args(flags)
        .arg(&c)
        .status()
        .ok()?;
    s.success().then_some(o)
}

/// Run `exe` under qemu-user `qemu`, with `prefix`'s loader if given.
fn run(qemu: &str, prefix: Option<&str>, exe: &Path) -> Option<std::process::Output> {
    let mut c = Command::new(qemu);
    if let Some(p) = prefix {
        if !Path::new(p)
            .join("lib/ld-linux-riscv64-lp64d.so.1")
            .exists()
        {
            return None;
        }
        c.arg("-L").arg(p);
    }
    c.arg(exe).output().ok()
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
/// (`__tls_get_addr`, in the loader; RISC-V has no local dynamic
/// relocations, its accesses use the general form), initial exec
/// through the GOT; GOT loads of data.
const TLS_PIC: &str = r#"
extern __thread int tgd;
static __thread int tld = 2;
static __thread int tld_bss;
extern __thread int tie __attribute__((tls_model("initial-exec")));
int plain = 5;
int get_gd(void) { return tgd; }
int get_ld(void) { return tld + tld_bss; }
void bump_ld(void) { tld += 10; tld_bss += 1; }
int get_ie(void) { return tie; }
int *addr_gd(void) { return &tgd; }
unsigned long divide(unsigned long a, unsigned long b) { return a / b; }
"#;

const MAIN: &str = r#"
__thread int tgd = 1;
__thread int tie = 3;
__thread int tle = 4;
__thread int tbss[16];
int get_gd(void);
int get_ld(void);
void bump_ld(void);
int get_ie(void);
int *addr_gd(void);
unsigned long divide(unsigned long, unsigned long);
extern int plain;
static int add(int a, int b) { return a + b; }
static int mul(int a, int b) { return a * b; }
static int (*const ops[])(int, int) = { add, mul };
static const char *names[] = { "add", "mul" };
int (*volatile fp)(int, int) = add;
extern void weak_missing(void) __attribute__((weak));

static void *thread(void *arg) {
    int *r = arg;
    r[0] = get_gd() + get_ld() + get_ie() + tle + tbss[3];
    *addr_gd() = 100;
    return 0;
}

int main(void) {
    volatile double x = 2.0;
    printf("sqrt %.3f cos %.3f\n", sqrt(x), cos(x - 2.0));
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
    printf("tls %d %d %d %d %d %d\n", get_gd(), get_ld(), get_ie(), tle, tbss[3], plain);
    int r[1];
    pthread_t t;
    pthread_create(&t, 0, thread, r);
    pthread_join(t, 0);
    printf("thread %d main %d %d\n", r[0], tgd, *addr_gd());
    printf("weak %d\n", weak_missing == 0);
    return 0;
}
"#;

const OUT: &str = "sqrt 1.414 cos 1.000\nadd 13\nmul 42\nfp 3 div 14\nmalloc 42\n\
                   tls 11 13 3 4 5 5\nthread 10 main 11 11\nweak 1\n";

/// A riscv64 PIE: machine, flags (RVC, double-float ABI), the merged
/// build attributes and their program header.
fn check_rv64(b: &[u8]) {
    assert!(b.starts_with(b"\x7fELF\x02\x01"));
    assert_eq!(u16_at(b, 16), 3, "ET_DYN");
    assert_eq!(u16_at(b, 18), 243);
    assert_eq!(u32_at(b, 48), 0x5, "e_flags");
    let (phoff, phnum) = (u64_at(b, 32) as usize, u16_at(b, 56) as usize);
    let attrs = (0..phnum)
        .map(|i| phoff + 56 * i)
        .find(|&p| u32_at(b, p) == 0x7000_0003)
        .expect("PT_RISCV_ATTRIBUTES");
    let off = u64_at(b, attrs + 8) as usize;
    assert_eq!(b[off], b'A');
    let body = &b[off..off + u64_at(b, attrs + 32) as usize];
    assert!(
        body.windows(6).any(|w| w == b"rv64i2"),
        "an ISA string in the attributes"
    );
}

#[test]
fn c_programs_against_glibc() {
    let Some(libc) = libc_dir() else {
        eprintln!("no riscv64 C runtime: skipped");
        return;
    };
    let Some(rt) = runtime(&libc) else {
        eprintln!("no riscv64 runtime (targets/build.sh {TRIPLE}): skipped");
        return;
    };
    let dir = scratch("c");
    let t = TargetSpec::builtin(TRIPLE).unwrap();
    let loader = Path::new(PREFIX).join("lib/ld-linux-riscv64-lp64d.so.1");
    // PIC thread-locals call `__tls_get_addr`: the loader is linked in.
    let variants: [(&str, &[&str], &[&str]); 4] = [
        ("O0", &["-O0", "-fPIE"], &["-O0", "-fPIE"]),
        ("O2", &["-O2", "-fPIE"], &["-O2", "-fPIE"]),
        (
            "norelax",
            &["-O2", "-fPIE", "-mno-relax"],
            &["-O1", "-fPIE", "-mno-relax"],
        ),
        ("pic", &["-O2", "-fPIE"], &["-O2", "-fPIC"]),
    ];
    for (v, main_flags, tls_flags) in variants {
        if v == "pic" && !loader.is_file() {
            eprintln!("{v}: no {}: skipped", loader.display());
            continue;
        }
        let main = format!("{DECLS}{MAIN}");
        let Some(m) = cc(
            &dir,
            &format!("main-{v}"),
            &main,
            "riscv64-linux-gnu",
            main_flags,
        ) else {
            eprintln!("{v}: no clang for RISC-V: skipped");
            continue;
        };
        let tls = cc(
            &dir,
            &format!("tls-{v}"),
            TLS_PIC,
            "riscv64-linux-gnu",
            tls_flags,
        )
        .unwrap();
        for strip in [false, true] {
            let exe = dir.join(format!("prog-{v}-{strip}"));
            // The runtime brings compiler-rt's helpers.
            let mut inputs = vec![m.as_os_str(), tls.as_os_str(), rt.as_os_str()];
            if v == "pic" {
                inputs.push(loader.as_os_str());
            }
            nabild::link_executable(&t, &inputs, &exe, strip, Some(&libc))
                .unwrap_or_else(|e| panic!("{v}: {e}"));
            check_rv64(&std::fs::read(&exe).unwrap());
            if let Some(o) = run("qemu-riscv64", Some(PREFIX), &exe) {
                assert!(o.status.success(), "{v}: {o:?}");
                assert_eq!(String::from_utf8_lossy(&o.stdout), OUT, "{v}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// nabi's runtime, whole: rustc's code (local exec TLS, GOT loads,
/// `eh_frame` with ADD / SUB pairs, ULEB128 pairs in its debug info),
/// compiler-rt, and its libc imports.
#[test]
fn runtime_archive() {
    let Some(libc) = libc_dir() else {
        eprintln!("no riscv64 C runtime: skipped");
        return;
    };
    let Some(rt) = runtime(&libc) else {
        eprintln!("no riscv64 runtime (targets/build.sh {TRIPLE}): skipped");
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
    let Some(m) = cc(&dir, "main", src, "riscv64-linux-gnu", &["-O2", "-fPIE"]) else {
        eprintln!("no clang for RISC-V: skipped");
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
        check_rv64(&std::fs::read(&exe).unwrap());
        if let Some(o) = run("qemu-riscv64", Some(PREFIX), &exe) {
            assert!(o.status.success(), "{o:?}");
            assert_eq!(
                String::from_utf8_lossy(&o.stdout),
                "cpus 1 cstr 5 zeroed 0 div 142857142857\n"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A freestanding riscv32 program: Linux system calls (qemu-user runs a
/// static image), its own thread pointer at the `PT_TLS` template found
/// through `__ehdr_start`, small data off `gp`, data pointers, 64-bit
/// arithmetic, compressed branches.
const BARE: &str = r#"
typedef unsigned int u32;
static long sys(long n, long a, long b, long c) {
    register long a0 asm("a0") = a;
    register long a1 asm("a1") = b;
    register long a2 asm("a2") = c;
    register long a7 asm("a7") = n;
    asm volatile("ecall" : "+r"(a0) : "r"(a1), "r"(a2), "r"(a7) : "memory");
    return a0;
}
struct phdr { u32 type, off, vaddr, paddr, filesz, memsz, flags, align; };
extern const unsigned char __ehdr_start[];
extern char __global_pointer$[];
__thread int tl = 40;
static const char msg[] = "hi rv32\n";
int counter = 2;
short small = 7;
static const char *strs[] = { msg, msg + 3 };
long long mul64(long long a, long long b) { return a * b; }
long long shl64(long long a, int n) { return a << n; }
static int put(const char *s, int n) { return sys(64, 1, (long)s, n); }
void _start(void) {
    asm volatile(".option push\n.option norelax\nla gp, __global_pointer$\n.option pop");
    u32 phoff = *(const u32 *)(__ehdr_start + 28);
    unsigned short phnum = *(const unsigned short *)(__ehdr_start + 44);
    const struct phdr *ph = (const struct phdr *)(__ehdr_start + phoff);
    for (int i = 0; i < phnum; i++)
        if (ph[i].type == 7)
            asm volatile("mv tp, %0" :: "r"(ph[i].vaddr));
    put(strs[0], 8);
    put(strs[1], 5);
    tl += counter + small;
    long long m = mul64(0x100000001ll, 3) + shl64(1, 40);
    int ok = m == 0x10300000003ll;
    sys(93, ok ? tl : 1, 0, 0);
}
"#;

/// `(p_type, p_vaddr, p_flags)` of every program header and the entry of
/// ELF32 image `b`.
fn layout32(b: &[u8]) -> (Vec<(u32, u32, u32)>, u32) {
    let (phoff, phnum) = (u32_at(b, 28) as usize, u16_at(b, 44) as usize);
    let ph = (0..phnum)
        .map(|i| {
            let p = phoff + 32 * i;
            (u32_at(b, p), u32_at(b, p + 8), u32_at(b, p + 24))
        })
        .collect();
    (ph, u32_at(b, 24))
}

#[test]
fn bare_rv32_static() {
    let dir = scratch("rv32");
    let t = TargetSpec::builtin(TRIPLE32).unwrap();
    assert!(nabild::links(&t));
    let flags = [
        "-march=rv32imac",
        "-mabi=ilp32",
        "-O2",
        "-ffreestanding",
        "-fno-pic",
        "-mno-relax",
        "-msmall-data-limit=8",
    ];
    let Some(o) = cc(&dir, "bare", BARE, "riscv32", &flags) else {
        eprintln!("no clang for RISC-V: skipped");
        return;
    };
    let exe = dir.join("bare");
    nabild::link_executable(&t, &[o.as_os_str()], &exe, true, None)
        .unwrap_or_else(|e| panic!("{e}"));
    let b = std::fs::read(&exe).unwrap();
    assert!(b.starts_with(b"\x7fELF\x01\x01"));
    assert_eq!(u16_at(&b, 16), 2, "ET_EXEC");
    assert_eq!(u16_at(&b, 18), 243);
    assert_eq!(u32_at(&b, 36), 0x1, "e_flags: RVC, soft float");
    let (ph, _) = layout32(&b);
    assert_eq!(ph.iter().find(|p| p.0 == 1).map(|p| p.1), Some(0x1_0000));
    assert!(ph.iter().any(|p| p.0 == 7), "PT_TLS");
    if let Some(lld) = std::env::var_os("NABI_LLD") {
        // Without a build ID: lld's is a 20-byte hash, nabi's 8 bytes.
        let args = |out: &Path| -> Vec<std::ffi::OsString> {
            let mut a: Vec<std::ffi::OsString> = [
                "-m",
                "elf32lriscv",
                "--eh-frame-hdr",
                "-static",
                "--gc-sections",
                "-o",
            ]
            .map(Into::into)
            .to_vec();
            a.push(out.into());
            a.push(o.clone().into());
            a
        };
        let mine = dir.join("bare.nobuildid");
        nabild::elf::link(&nabild::elf::parse_args(&args(&mine)).unwrap()).unwrap();
        let ref_exe = dir.join("bare.lld");
        let s = Command::new(lld)
            .arg("--no-relax")
            .args(args(&ref_exe))
            .status()
            .unwrap();
        assert!(s.success());
        let (ph, entry) = layout32(&std::fs::read(&mine).unwrap());
        let (rph, rentry) = layout32(&std::fs::read(&ref_exe).unwrap());
        assert_eq!(ph, rph, "program headers as lld's");
        assert_eq!(entry, rentry, "entry as lld's");
    }
    if let Ok(r) = Command::new("qemu-riscv32").arg(&exe).output() {
        assert_eq!(String::from_utf8_lossy(&r.stdout), "hi rv32\nrv32\n");
        assert_eq!(r.status.code(), Some(49), "{r:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Objects of another float ABI do not mix.
#[test]
fn float_abi_mismatch_fails() {
    let dir = scratch("abi");
    let t = TargetSpec::builtin(TRIPLE32).unwrap();
    let src = "void _start(void) {}\n";
    let Some(a) = cc(
        &dir,
        "soft",
        src,
        "riscv32",
        &["-march=rv32imac", "-mabi=ilp32"],
    ) else {
        eprintln!("no clang for RISC-V: skipped");
        return;
    };
    let Some(f) = cc(
        &dir,
        "hard",
        "int g(void) { return 1; }\n",
        "riscv32",
        &["-march=rv32imafc", "-mabi=ilp32f"],
    ) else {
        return;
    };
    let exe = dir.join("mix");
    let e = nabild::link_executable(&t, &[a.as_os_str(), f.as_os_str()], &exe, false, None)
        .unwrap_err();
    assert!(e.contains("floating-point ABI"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without `_start` a static image starts at 0, as lld leaves it.
#[test]
fn bare_rv32_without_entry() {
    let dir = scratch("noentry");
    let t = TargetSpec::builtin(TRIPLE32).unwrap();
    let Some(o) = cc(
        &dir,
        "f",
        "int f(void) { return 1; }\n",
        "riscv32",
        &["-march=rv32imac", "-mabi=ilp32"],
    ) else {
        eprintln!("no clang for RISC-V: skipped");
        return;
    };
    let exe = dir.join("f");
    nabild::link_executable(&t, &[o.as_os_str()], &exe, false, None)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(layout32(&std::fs::read(&exe).unwrap()).1, 0);
    let _ = std::fs::remove_dir_all(&dir);
}
