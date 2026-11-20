//! The ELF linker for 32-bit ARM: C programs (A32 and T32 code calling
//! each other and the PLT, TLS in every model, libm, threads) and nabi's
//! runtime into PIEs against the armhf glibc runtime `targets/build.sh`
//! makes. The tests need clang with the ARM target and that runtime
//! (`NABI_ARMV7_LIBC`, else the cargo target directory's
//! `lib/nabi/targets/armv7-unknown-linux-gnueabihf`); executables run
//! under `qemu-arm` with `/usr/arm-linux-gnueabihf`'s loader, when there.

use nabic_target::TargetSpec;
use std::path::{Path, PathBuf};
use std::process::Command;

const TRIPLE: &str = "armv7-unknown-linux-gnueabihf";
const PREFIX: &str = "/usr/arm-linux-gnueabihf";

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(&format!("link-elf-arm-{name}"))
}

const fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// The armhf C runtime, if built.
fn libc_dir() -> Option<PathBuf> {
    let d = std::env::var_os("NABI_ARMV7_LIBC")
        .map(PathBuf::from)
        .or_else(|| {
            let t = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
                || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
                PathBuf::from,
            );
            Some(t.join("lib/nabi/targets").join(TRIPLE))
        })?;
    d.join("Scrt1.o").is_file().then_some(d)
}

/// `clang --target=armv7-linux-gnueabihf -c` of `src` with `flags`, or
/// `None` without such a clang.
fn cc(dir: &Path, name: &str, src: &str, flags: &[&str]) -> Option<PathBuf> {
    let c = dir.join(format!("{name}.c"));
    let o = dir.join(format!("{name}.o"));
    std::fs::write(&c, src).unwrap();
    let s = Command::new("clang")
        .args([
            "--target=armv7-linux-gnueabihf",
            "-mfpu=neon",
            "-c",
            "-g",
            "-o",
        ])
        .arg(&o)
        .args(flags)
        .arg(&c)
        .status()
        .ok()?;
    s.success().then_some(o)
}

/// Run `exe` under `qemu-arm`, when it and the armhf loader are there.
fn run(exe: &Path) -> Option<std::process::Output> {
    if !Path::new(PREFIX).join("lib/ld-linux-armhf.so.3").exists() {
        return None;
    }
    Command::new("qemu-arm")
        .arg("-L")
        .arg(PREFIX)
        .arg(exe)
        .output()
        .ok()
}

/// The program headers' `PT_ARM_EXIDX`: its table is sorted by address
/// (PREL31 from each entry) and ends with `EXIDX_CANTUNWIND`.
fn check_exidx(b: &[u8]) {
    let (phoff, phnum) = (u32_at(b, 28) as usize, u16_at(b, 44) as usize);
    let p = (0..phnum)
        .map(|i| phoff + 32 * i)
        .find(|&p| u32_at(b, p) == 0x7000_0001)
        .expect("PT_ARM_EXIDX");
    let (off, addr, size) = (
        u32_at(b, p + 4) as usize,
        u32_at(b, p + 8),
        u32_at(b, p + 16) as usize,
    );
    assert!(size >= 8 && size % 8 == 0);
    let mut last = 0u32;
    for k in 0..size / 8 {
        let w = u32_at(b, off + 8 * k);
        let rel = ((w << 1) as i32 >> 1) as u32;
        let to = (addr + 8 * k as u32).wrapping_add(rel);
        assert!(to >= last, "unsorted at entry {k}: {to:#x} < {last:#x}");
        last = to;
    }
    assert_eq!(u32_at(b, off + size - 4), 1, "EXIDX_CANTUNWIND last");
}

/// No libc headers: what the programs use, declared.
const DECLS: &str = r"
typedef unsigned int size_t;
typedef unsigned long pthread_t;
int printf(const char *, ...);
void *malloc(size_t);
void free(void *);
double sqrt(double);
double cos(double);
int pthread_create(pthread_t *, const void *, void *(*)(void *), void *);
int pthread_join(pthread_t, void **);
";

/// TLS of every model the compiler keeps in PIC code: general and
/// local dynamic (`__tls_get_addr`, in the loader), initial exec through
/// the GOT; a division (a run-time ABI helper).
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
unsigned divide(unsigned a, unsigned b) { return a / b; }
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
unsigned divide(unsigned, unsigned);
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
    printf("fp %d div %u\n", fp(1, 2), divide(100, 7));
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

#[test]
fn c_programs_against_glibc() {
    let Some(libc) = libc_dir() else {
        eprintln!("no armv7 C runtime (targets/build.sh {TRIPLE}): skipped");
        return;
    };
    let dir = scratch("c");
    let t = TargetSpec::builtin(TRIPLE).unwrap();
    // A32 and T32 objects, either way round: `blx` and thunks between.
    // PIC thread-locals call `__tls_get_addr`: the loader is linked in.
    let loader = Path::new(PREFIX).join("lib/ld-linux-armhf.so.3");
    let variants: [(&str, &[&str], &[&str]); 5] = [
        (
            "arm-O0",
            &["-O0", "-fPIE", "-marm"],
            &["-O0", "-fPIE", "-marm"],
        ),
        (
            "arm-O2",
            &["-O2", "-fPIE", "-marm"],
            &["-O2", "-fPIE", "-mthumb"],
        ),
        (
            "thumb-O2",
            &["-O2", "-fPIE", "-mthumb"],
            &["-O2", "-fPIE", "-marm"],
        ),
        (
            "thumb-O0",
            &["-O0", "-fPIE", "-mthumb"],
            &["-O0", "-fPIE", "-mthumb"],
        ),
        (
            "pic",
            &["-O2", "-fPIE", "-mthumb"],
            &["-O2", "-fPIC", "-marm"],
        ),
    ];
    let rt = libc.join("libnabir.a");
    for (v, main_flags, tls_flags) in variants {
        if v == "pic" && !loader.is_file() {
            eprintln!("{v}: no {}: skipped", loader.display());
            continue;
        }
        let main = format!("{DECLS}{MAIN}");
        let Some(m) = cc(&dir, &format!("main-{v}"), &main, main_flags) else {
            eprintln!("{v}: no clang for ARM: skipped");
            continue;
        };
        let tls = cc(&dir, &format!("tls-{v}"), TLS_PIC, tls_flags).unwrap();
        for strip in [false, true] {
            let exe = dir.join(format!("prog-{v}-{strip}"));
            // The runtime brings the run-time ABI helpers.
            let mut inputs = vec![m.as_os_str(), tls.as_os_str(), rt.as_os_str()];
            if v == "pic" {
                inputs.push(loader.as_os_str());
            }
            nabild::link_executable(&t, &inputs, &exe, strip, Some(&libc))
                .unwrap_or_else(|e| panic!("{v}: {e}"));
            let b = std::fs::read(&exe).unwrap();
            assert!(b.starts_with(b"\x7fELF\x01\x01"));
            assert_eq!(u16_at(&b, 16), 3, "ET_DYN");
            assert_eq!(u16_at(&b, 18), 40);
            assert_eq!(u32_at(&b, 36), 0x0500_0400, "e_flags");
            check_exidx(&b);
            if let Some(o) = run(&exe) {
                assert!(o.status.success(), "{v}: {o:?}");
                assert_eq!(String::from_utf8_lossy(&o.stdout), OUT, "{v}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// nabi's runtime, whole: rustc's A32 code with compiler-rt's T32
/// helpers (`b` between them through thunks), local exec TLS, GOT
/// literals, and its libc imports.
#[test]
fn runtime_archive() {
    let Some(libc) = libc_dir() else {
        eprintln!("no armv7 C runtime (targets/build.sh {TRIPLE}): skipped");
        return;
    };
    let dir = scratch("rt");
    let t = TargetSpec::builtin(TRIPLE).unwrap();
    let src = r#"
typedef unsigned int size_t;
int printf(const char *, ...);
size_t nabi_cpu_count(void);
size_t nabi_cstr_len(const char *);
unsigned char *nabi_alloc_raw(size_t, size_t, int);
unsigned long long __aeabi_uldivmod(unsigned long long, unsigned long long);
int main(void) {
    unsigned char *p = nabi_alloc_raw(16, 8, 1);
    volatile unsigned long long a = 1000000000000ull, b = 7;
    printf("cpus %d cstr %u zeroed %d div %llu\n", nabi_cpu_count() > 0,
           nabi_cstr_len("hello"), p[15], a / b);
    return 0;
}
"#;
    let Some(m) = cc(&dir, "main", src, &["-O2", "-fPIE", "-mthumb"]) else {
        eprintln!("no clang for ARM: skipped");
        return;
    };
    let rt = libc.join("libnabir.a");
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
        check_exidx(&std::fs::read(&exe).unwrap());
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
