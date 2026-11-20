//! The ELF linker for i386: a hand-written ELF32 object with REL
//! relocations into a static executable, and C programs (TLS in every
//! model, the PLT, libm, threads) into PIEs against the i686 glibc
//! runtime `targets/build.sh` makes. The C tests need clang with the x86
//! target and that runtime (`NABI_I686_LIBC`, else the cargo target
//! directory's `lib/nabi/targets/i686-unknown-linux-gnu`); executables
//! run on `x86_64` Linux only.

use nabic_target::TargetSpec;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(&format!("link-elf-i686-{name}"))
}

const RUN: bool = cfg!(all(target_os = "linux", target_arch = "x86_64"));

fn u32s(b: &mut Vec<u8>, v: &[u32]) {
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
}

/// An ELF32 i386 object: `_start` calls `f` (`R_386_PC32`, implicit
/// addend -4) in a section of its own, which exits with the word at
/// `.data` (`R_386_32` against the section symbol).
fn object(code: u8) -> Vec<u8> {
    // call f; hlt
    let text: &[u8] = &[0xe8, 0xfc, 0xff, 0xff, 0xff, 0xf4];
    // mov eax, 1; mov ebx, [val]; int 0x80
    let text_f: &[u8] = &[0xb8, 1, 0, 0, 0, 0x8b, 0x1d, 0, 0, 0, 0, 0xcd, 0x80];
    let data = [code, 0, 0, 0];
    let rel_text: &[u32] = &[1, (3 << 8) | 2];
    let rel_f: &[u32] = &[7, (1 << 8) | 1];
    let strtab = b"\0_start\0f\0";
    let shstrtab =
        b"\0.text\0.text.f\0.data\0.rel.text\0.rel.text.f\0.symtab\0.strtab\0.shstrtab\0";
    let mut syms = vec![0u8; 16];
    // .data section symbol, _start, f
    for (name, info, shndx) in [(0u32, 0x03u8, 3u16), (1, 0x12, 1), (8, 0x12, 2)] {
        u32s(&mut syms, &[name, 0, 0]);
        syms.extend_from_slice(&[info, 0]);
        syms.extend_from_slice(&shndx.to_le_bytes());
    }
    let mut body = Vec::new();
    let at = |b: &mut Vec<u8>, part: &[u8], align: usize| {
        b.resize((52 + b.len()).next_multiple_of(align) - 52, 0);
        let off = 52 + b.len();
        b.extend_from_slice(part);
        off
    };
    let rel_bytes = |r: &[u32]| r.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let o_text = at(&mut body, text, 16);
    let o_f = at(&mut body, text_f, 16);
    let o_data = at(&mut body, &data, 4);
    let o_rt = at(&mut body, &rel_bytes(rel_text), 4);
    let o_rf = at(&mut body, &rel_bytes(rel_f), 4);
    let o_sym = at(&mut body, &syms, 4);
    let o_str = at(&mut body, strtab, 1);
    let o_shstr = at(&mut body, shstrtab, 1);
    let sh_at = (52 + body.len()).next_multiple_of(4);
    let mut b = vec![0x7f, b'E', b'L', b'F', 1, 1, 1, 0];
    b.resize(16, 0);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes());
    u32s(&mut b, &[1, 0, 0, sh_at as u32, 0]);
    for v in [52u16, 0, 0, 40, 9, 8] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&body);
    b.resize(sh_at, 0);
    // name, type, flags, addr, offset, size, link, info, align, entsize
    let sects: [[u32; 10]; 9] = [
        [0; 10],
        [1, 1, 6, 0, o_text as u32, text.len() as u32, 0, 0, 16, 0],
        [7, 1, 6, 0, o_f as u32, text_f.len() as u32, 0, 0, 16, 0],
        [15, 1, 3, 0, o_data as u32, 4, 0, 0, 4, 0],
        [21, 9, 0x40, 0, o_rt as u32, 8, 6, 1, 4, 8],
        [31, 9, 0x40, 0, o_rf as u32, 8, 6, 2, 4, 8],
        [43, 2, 0, 0, o_sym as u32, syms.len() as u32, 7, 2, 4, 16],
        [51, 3, 0, 0, o_str as u32, strtab.len() as u32, 0, 0, 1, 0],
        [
            59,
            3,
            0,
            0,
            o_shstr as u32,
            shstrtab.len() as u32,
            0,
            0,
            1,
            0,
        ],
    ];
    for s in sects {
        u32s(&mut b, &s);
    }
    b
}

const fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[test]
fn static_elf32_executable_with_rel_relocations() {
    let dir = scratch("static");
    let o = dir.join("start.o");
    std::fs::write(&o, object(9)).unwrap();
    let exe = dir.join("start");
    let args = [
        "-m".into(),
        "elf_i386".into(),
        "-static".into(),
        "--gc-sections".into(),
        "--build-id".into(),
        "-o".into(),
        exe.clone().into_os_string(),
        o.into_os_string(),
    ];
    let opts = nabild::elf::parse_args(&args).unwrap();
    nabild::elf::link(&opts).unwrap();
    let b = std::fs::read(&exe).unwrap();
    assert!(b.starts_with(b"\x7fELF\x01\x01"));
    assert_eq!(u16_at(&b, 16), 2, "ET_EXEC");
    assert_eq!(u16_at(&b, 18), 3);
    let entry = u32_at(&b, 24);
    let (phoff, phnum) = (u32_at(&b, 28) as usize, u16_at(&b, 44) as usize);
    assert_eq!(u16_at(&b, 42), 32);
    let exec = (0..phnum)
        .map(|i| phoff + 32 * i)
        .find(|&p| u32_at(&b, p) == 1 && u32_at(&b, p + 24) & 1 != 0);
    let p = exec.expect("an executable PT_LOAD");
    let (vaddr, memsz) = (u32_at(&b, p + 8), u32_at(&b, p + 20));
    assert!(entry >= vaddr && entry < vaddr + memsz, "{entry:#x}");
    if RUN {
        let s = Command::new(&exe).status().unwrap();
        assert_eq!(s.code(), Some(9));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The i686 C runtime, if built.
fn libc_dir() -> Option<PathBuf> {
    let d = std::env::var_os("NABI_I686_LIBC")
        .map(PathBuf::from)
        .or_else(|| {
            let t = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
                || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
                PathBuf::from,
            );
            Some(t.join("lib/nabi/targets/i686-unknown-linux-gnu"))
        })?;
    d.join("Scrt1.o").is_file().then_some(d)
}

/// A C compiler for i386 Linux: clang, or GCC on Linux.
#[derive(Clone, Copy)]
enum Cc {
    Clang,
    Gcc,
}

/// `clang --target=i686-linux-gnu -c` of `src` with `flags`, or `None`
/// without such a clang.
fn cc(dir: &Path, name: &str, src: &str, flags: &[&str]) -> Option<PathBuf> {
    compile(Cc::Clang, dir, name, src, flags)
}

fn compile(with: Cc, dir: &Path, name: &str, src: &str, flags: &[&str]) -> Option<PathBuf> {
    let c = dir.join(format!("{name}.c"));
    let o = dir.join(format!("{name}.o"));
    std::fs::write(&c, src).unwrap();
    let (cmd, target) = match with {
        Cc::Clang => ("clang", "--target=i686-linux-gnu"),
        Cc::Gcc if cfg!(target_os = "linux") => ("gcc", "-m32"),
        Cc::Gcc => return None,
    };
    let s = Command::new(cmd)
        .args([target, "-c", "-g", "-o"])
        .arg(&o)
        .args(flags)
        .arg(&c)
        .status()
        .ok()?;
    s.success().then_some(o)
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
/// local dynamic, initial exec through the GOT.
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
    printf("fp %d\n", fp(1, 2));
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

const OUT: &str = "sqrt 1.414 cos 1.000\nadd 13\nmul 42\nfp 3\nmalloc 42\n\
                   tls 11 13 3 4 5 5\nthread 10 main 11 11\nweak 1\n";

#[test]
fn c_programs_against_glibc() {
    let Some(libc) = libc_dir() else {
        eprintln!("no i686 C runtime (targets/build.sh i686-unknown-linux-gnu): skipped");
        return;
    };
    let dir = scratch("c");
    let t = TargetSpec::builtin("i686-unknown-linux-gnu").unwrap();
    // GCC: `leal x@tlsgd(,%ebx,1)`, `mov x@GOT(%ebx)` and TLS
    // descriptors.
    let variants: [(&str, Cc, &[&str], &[&str]); 6] = [
        ("O0", Cc::Clang, &["-O0", "-fPIE"], &["-O0", "-fPIC"]),
        ("O2", Cc::Clang, &["-O2", "-fPIE"], &["-O2", "-fPIC"]),
        (
            "noplt",
            Cc::Clang,
            &["-O2", "-fPIE", "-fno-plt"],
            &["-O2", "-fPIC", "-fno-plt"],
        ),
        ("gcc", Cc::Gcc, &["-O2", "-fPIE"], &["-O2", "-fPIC"]),
        (
            "gcc-noplt",
            Cc::Gcc,
            &["-O0", "-fPIE", "-fno-plt"],
            &["-O0", "-fPIC", "-fno-plt"],
        ),
        (
            "gcc-desc",
            Cc::Gcc,
            &["-O2", "-fPIE", "-mtls-dialect=gnu2"],
            &["-O2", "-fPIC", "-mtls-dialect=gnu2"],
        ),
    ];
    for (v, with, main_flags, tls_flags) in variants {
        let main = format!("{DECLS}{MAIN}");
        let Some(m) = compile(with, &dir, &format!("main-{v}"), &main, main_flags) else {
            eprintln!("{v}: no C compiler for i686: skipped");
            continue;
        };
        let tls = compile(with, &dir, &format!("tls-{v}"), TLS_PIC, tls_flags).unwrap();
        for strip in [false, true] {
            let exe = dir.join(format!("prog-{v}-{strip}"));
            nabild::link_executable(&t, &[&m, &tls], &exe, strip, Some(&libc))
                .unwrap_or_else(|e| panic!("{v}: {e}"));
            let b = std::fs::read(&exe).unwrap();
            assert!(b.starts_with(b"\x7fELF\x01\x01"));
            assert_eq!(u16_at(&b, 16), 3, "ET_DYN");
            if RUN {
                let o = Command::new(&exe).output().unwrap();
                assert!(o.status.success(), "{v}: {o:?}");
                assert_eq!(String::from_utf8_lossy(&o.stdout), OUT, "{v}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// nabi's runtime, whole: rustc's PIC code (`GOTOFF`, `GOTPC`, `PLT32`,
/// local exec TLS) and its libc imports.
#[test]
fn runtime_archive() {
    let Some(libc) = libc_dir() else {
        eprintln!("no i686 C runtime (targets/build.sh i686-unknown-linux-gnu): skipped");
        return;
    };
    let dir = scratch("rt");
    let t = TargetSpec::builtin("i686-unknown-linux-gnu").unwrap();
    let src = r#"
typedef unsigned int size_t;
int printf(const char *, ...);
size_t nabi_cpu_count(void);
size_t nabi_cstr_len(const char *);
unsigned char *nabi_alloc_raw(size_t, size_t, int);
int main(void) {
    unsigned char *p = nabi_alloc_raw(16, 8, 1);
    printf("cpus %d cstr %u zeroed %d\n", nabi_cpu_count() > 0, nabi_cstr_len("hello"), p[15]);
    return 0;
}
"#;
    let Some(m) = cc(&dir, "main", src, &["-O2", "-fPIE"]) else {
        eprintln!("no clang for i686: skipped");
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
        if RUN {
            let o = Command::new(&exe).output().unwrap();
            assert!(o.status.success(), "{o:?}");
            assert_eq!(
                String::from_utf8_lossy(&o.stdout),
                "cpus 1 cstr 5 zeroed 0\n"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Data and thread-locals of a shared object: `GLOB_DAT`, symbolic
/// `R_386_32` and `TLS_TPOFF` dynamic relocations, general dynamic TLS
/// relaxed to initial exec. GCC (and its linker) make the library.
#[test]
fn shared_object_data_and_tls() {
    let Some(libc) = libc_dir() else {
        eprintln!("no i686 C runtime (targets/build.sh i686-unknown-linux-gnu): skipped");
        return;
    };
    let dir = scratch("dso");
    let lib = r"
__thread int dso_tls = 7;
__thread int dso_tls2[4] = { 1, 2, 3, 4 };
int dso_data = 9;
int dso_get(void) { return dso_tls; }
";
    let Some(lo) = compile(Cc::Gcc, &dir, "lib", lib, &["-O2", "-fPIC"]) else {
        eprintln!("no GCC for i686: skipped");
        return;
    };
    let so = dir.join("libdso.so");
    let s = Command::new("gcc")
        .args([
            "-m32",
            "-shared",
            "-nostdlib",
            "-Wl,-soname,libdso.so",
            "-o",
        ])
        .arg(&so)
        .arg(&lo)
        .status()
        .unwrap();
    assert!(s.success());
    let main = r#"
int printf(const char *, ...);
extern __thread int dso_tls;
extern __thread int dso_tls2[4] __attribute__((tls_model("initial-exec")));
extern int dso_data;
int dso_get(void);
int *ptr = &dso_data;
int main(void) {
    dso_tls += 1;
    printf("%d %d %d %d %d\n", dso_get(), dso_tls, dso_tls2[2], *ptr, dso_data);
    return 0;
}
"#;
    let m = compile(Cc::Gcc, &dir, "main", main, &["-O2", "-fPIC"]).unwrap();
    let t = TargetSpec::builtin("i686-unknown-linux-gnu").unwrap();
    let exe = dir.join("main");
    let inputs = [
        m.as_os_str(),
        so.as_os_str(),
        "-rpath".as_ref(),
        dir.as_os_str(),
    ];
    nabild::link_executable(&t, &inputs, &exe, false, Some(&libc))
        .unwrap_or_else(|e| panic!("{e}"));
    if RUN {
        let o = Command::new(&exe).output().unwrap();
        assert!(o.status.success(), "{o:?}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "8 8 3 9 9\n");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
