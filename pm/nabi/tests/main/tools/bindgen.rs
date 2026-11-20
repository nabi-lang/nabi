//! `nabi bindgen`.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

/// `nabi bindgen <args>` in `dir`.
pub(crate) fn bindgen(dir: &Path, args: &[&str]) -> Output {
    let mut args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    if !args.iter().any(|a| a == "--target") {
        let dirs = host_c_includes();
        if !dirs.is_empty() && !args.iter().any(|a| a == "--") {
            args.push("--".into());
        }
        for d in dirs {
            args.extend(["-isystem".into(), d.to_string_lossy().into_owned()]);
        }
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    nabi_in(None, dir, &[&["bindgen"], &args[..]].concat())
}

/// Linux: the host C compiler's include directories but those of its
/// built-in headers (nabic has its own), for the C library headers the
/// FHS defaults miss (`NixOS` has no `/usr/include`). FreeBSD: base's
/// `/usr/include`.
pub(crate) fn host_c_includes() -> Vec<PathBuf> {
    if cfg!(target_os = "freebsd") {
        return vec![PathBuf::from("/usr/include")];
    }
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let Ok(out) = Command::new("cc")
        .args(["-xc", "-E", "-v", "-"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    text(&out.stderr)
        .lines()
        .skip_while(|l| !l.starts_with("#include <...>"))
        .skip(1)
        .take_while(|l| l.starts_with(' '))
        .map(|l| PathBuf::from(l.trim()))
        // Not the compiler's own headers (stddef.h without stdio.h); musl
        // keeps a stddef.h of its own beside stdio.h.
        .filter(|d| !d.join("stddef.h").is_file() || d.join("stdio.h").is_file())
        .collect()
}

pub(crate) const BINDGEN_H: &str = r#"#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#define TINY_VERSION 3
#define TINY_NAME "tiny"
#define TINY_MASK (1u << 4)
#define TINY_BIG 0xffffffffffffffffULL
#define TINY_NEG (-2)
#define TINY_PI 3.25
#define TINY_SUM (TINY_VERSION + 4)
#define TINY_FN(x) ((x) + 1)
#define TINY_API

typedef int32_t tiny_id;
typedef struct tiny_point { int x; int y; } tiny_point;
typedef struct { double w, h; } tiny_size;
struct tiny_opaque;
typedef struct tiny_opaque tiny_handle;
enum tiny_color { TINY_RED, TINY_GREEN = 5, TINY_BLUE };
typedef enum { TINY_A = -1, TINY_B = 7 } tiny_mode;
enum { TINY_ANON_X = 100 };
union tiny_value { int i; float f; uint8_t bytes[8]; };
struct tiny_flags { unsigned a : 3; unsigned b : 5; int c; unsigned d : 1; };
struct tiny_nested { struct { int u, v; } inner; union { long long l; char c; }; int tail[]; };
#pragma pack(push, 1)
struct tiny_packed { char c; int i; };
#pragma pack(pop)
struct tiny_aligned { _Alignas(16) char c; };
typedef int (*tiny_cb)(int, void *);
typedef void tiny_fn_t(int);

extern int tiny_counter;
extern const char *const tiny_names[];

tiny_point tiny_add(tiny_point a, tiny_point b);
int tiny_apply(tiny_cb cb, void *data, int n);
size_t tiny_len(const char *s);
int tiny_sum(int n, ...);
tiny_handle *tiny_open(const char *path, FILE *log);
int tiny_close(tiny_handle *h);
void tiny_on(tiny_fn_t *f);
static inline int tiny_twice(int x) { return 2 * x; }
int match(int type, int fn);
enum tiny_color tiny_next(enum tiny_color c, tiny_mode m);
size_t tiny_size_of(int which);
"#;

pub(crate) const BINDGEN_C: &str = r#"#include "tiny.h"
#include <stdarg.h>
#include <string.h>

int tiny_counter = 41;
const char *const tiny_names[] = {"zero", "one"};

struct tiny_opaque { int fd; };
static struct tiny_opaque the_handle = {9};

tiny_point tiny_add(tiny_point a, tiny_point b) {
    tiny_point r = {a.x + b.x, a.y + b.y};
    return r;
}

int tiny_apply(tiny_cb cb, void *data, int n) { return cb ? cb(n, data) : -1; }

size_t tiny_len(const char *s) { return strlen(s); }

int tiny_sum(int n, ...) {
    va_list ap;
    int s = 0;
    va_start(ap, n);
    for (int i = 0; i < n; i++) s += va_arg(ap, int);
    va_end(ap);
    return s;
}

tiny_handle *tiny_open(const char *path, FILE *log) {
    (void)log;
    return path ? &the_handle : 0;
}

int tiny_close(tiny_handle *h) { return h->fd; }

void tiny_on(tiny_fn_t *f) { f(7); }

int match(int type, int fn) { return type * 10 + fn; }

enum tiny_color tiny_next(enum tiny_color c, tiny_mode m) { return m == TINY_B ? TINY_BLUE : c; }

size_t tiny_size_of(int which) {
    size_t sizes[] = {sizeof(struct tiny_flags), sizeof(struct tiny_nested),
                      sizeof(struct tiny_packed), sizeof(struct tiny_aligned),
                      sizeof(union tiny_value), sizeof(tiny_size)};
    return sizes[which];
}
"#;

pub(crate) const BINDGEN_NABI: &str = r#"mod tiny_sys;
use tiny_sys::*;
use bare::ffi::CString;
use bare::mem::size_of;
use bare::ptr::CPtr;

extern "C" fn twice(n: i32, data: CPtr<u8>) -> i32 {
    n * 2 + (data.addr() as i32)
}

extern "C" fn seen(x: i32) {
    println(f"seen {x}");
}

fn main() {
    println(f"{TINY_VERSION} {TINY_NAME} {TINY_MASK} {TINY_BIG} {TINY_SUM} {TINY_NEG} {TINY_PI}");
    let p = unsafe { tiny_add(tiny_point { x: 1, y: 2 }, tiny_point { x: 10, y: 20 }) };
    println(f"add {p.x} {p.y}");
    println(f"apply {unsafe { tiny_apply(twice, CPtr::null(), 21) }}");
    let s = try! CString::new("hello");
    let path = CPtr::from_const(s.as_ptr());
    println(f"len {unsafe { tiny_len(path) }} sum {unsafe { tiny_sum(3, 1, 2, 3) }}");
    let h = unsafe { tiny_open(path, CPtr::null()) };
    println(f"open {h.is_null()} {unsafe { tiny_close(h) }}");
    unsafe { tiny_on(seen) };
    println(f"match {unsafe { match_(4, 2) }}");
    println(f"next {unsafe { tiny_next(TINY_RED, TINY_B) }} {TINY_GREEN} {TINY_A} {TINY_ANON_X}");
    println(f"counter {unsafe { tiny_counter }}");
    let nabi = [size_of::<tiny_flags>(), size_of::<tiny_nested>(), size_of::<tiny_packed>(), size_of::<tiny_aligned>(), size_of::<tiny_value>(), size_of::<tiny_size>()];
    let same = 0;
    for i in 0..6 {
        if nabi[i] == unsafe { tiny_size_of(i as i32) } {
            same += 1;
        }
    }
    println(f"sizes {same} of 6");
}
"#;

/// Bindings to a small C library: generated, compiled and run against
/// it, their layouts checked against clang's `sizeof`.
#[test]
fn bindgen_binds_a_c_library() {
    let env = setup();
    if crate::tools::test_target().is_some() {
        return;
    }
    let dir = scratch("bindgen");
    put(dir.join("tiny.h"), BINDGEN_H).unwrap();
    put(dir.join("tiny.c"), BINDGEN_C).unwrap();
    put(dir.join("app/src/main.nabi"), BINDGEN_NABI).unwrap();
    let out = bindgen(&dir, &["tiny.h", "-o", "app/src/tiny_sys.nabi"]);
    assert_exit(&out, 0);
    let nabi = fs::read_to_string(dir.join("app/src/tiny_sys.nabi")).unwrap();
    for want in [
        "use bare::ptr::CPtr;",
        "pub const TINY_NAME: str = \"tiny\";",
        "pub const TINY_SUM: i32 = 7;",
        "#[repr(C)]\npub struct tiny_point {\n    pub x: i32;\n    pub y: i32;\n}",
        "pub struct tiny_opaque;",
        "pub type tiny_handle = tiny_opaque;",
        "pub union tiny_value {",
        "pub _bitfield0: [u8; 4];",
        "#[repr(packed)]\npub struct tiny_packed {",
        "#[repr(C, align(16))]\npub struct tiny_aligned {",
        "pub _anon0: tiny_nested_anon0;",
        "pub tail: [i32; 0];",
        "pub type tiny_cb = unsafe extern \"C\" fn(i32, CPtr<u8>) -> i32 | none;",
        "// skipped fn `tiny_twice`: static or inline, no symbol to link",
        "pub static tiny_names: [CPtr<c_char>; 0];",
        "pub static mut tiny_counter: i32;",
        "pub fn tiny_sum(n: i32, ...) -> i32;",
        "pub fn tiny_on(f: unsafe extern \"C\" fn(i32) | none);",
        "#[link_name = \"match\"]\n    pub fn match_(type_: i32, fn_: i32) -> i32;",
        "pub fn tiny_len(s: CPtr<c_char>) -> usize;",
    ] {
        assert!(nabi.contains(want), "missing {want:?} in:\n{nabi}");
    }
    // `FILE` comes along from <stdio.h>, the rest of it does not.
    assert!(
        nabi.contains("pub type FILE = ") || nabi.contains("pub struct FILE"),
        "{nabi}"
    );
    assert!(!nabi.contains("fn printf"), "{nabi}");

    // The library, as the platform's archive; position-independent, as
    // a PIE needs (FreeBSD's clang does not default to it).
    let run_ok = |c: &mut Command| assert_exit(&c.current_dir(&dir).output().unwrap(), 0);
    let pic: &[&str] = if cfg!(windows) { &[] } else { &["-fPIC"] };
    run_ok(
        Command::new("clang")
            .args(pic)
            .args(["-c", "tiny.c", "-o", "tiny.o"]),
    );
    if cfg!(windows) {
        run_ok(Command::new("lib").args(["/nologo", "/OUT:tiny.lib", "tiny.o"]));
    } else {
        run_ok(Command::new("ar").args(["rcs", "libtiny.a", "tiny.o"]));
    }
    let lib_dir = dir.to_str().unwrap();
    assert_exit(
        &nabi_in(
            Some(&env),
            &dir,
            &["-q", "build", "app", "-L", lib_dir, "-l", "static=tiny"],
        ),
        0,
    );
    let run = Command::new(dir.join("app/target/debug").join(exe_name("app")))
        .output()
        .unwrap();
    assert_exit(&run, 0);
    assert_eq!(
        text(&run.stdout),
        "3 tiny 16 18446744073709551615 7 -2 3.25\nadd 11 22\napply 42\nlen 5 sum 6\n\
         open false 9\nseen 7\nmatch 42\nnext 6 5 -1 100\ncounter 41\n\
         sizes 6 of 6\n"
    );

    // A header clang rejects is its error, exit 1.
    put(dir.join("bad.h"), "int f(;\n").unwrap();
    let out = bindgen(&dir, &["bad.h"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("bad.h:1:7: error"),
        "{}",
        text(&out.stderr)
    );
}

pub(crate) const BINDGEN_UNSUPPORTED_H: &str = r"#include <stddef.h>
typedef int (*vcb)(const char *fmt, ...);
__int128 add128(__int128 a);
unsigned __int128 uadd128(unsigned __int128 a);
_Float16 half(_Float16 x);
long double ld(long double x);
struct wide { long double x; int y; };
struct inner { __int128 v; };
struct outer { struct inner i; int k; };
struct plain { int a; double b; };
void take_wide(struct wide w);
void take_wide_ptr(struct wide *w);
struct outer give_outer(void);
void take_plain(struct plain p, size_t n);
typedef void (*wide_cb)(struct wide);
struct has_vcb { vcb f; int n; };
int use_vcb(vcb f);
#define LD_ONE 1.0L
#define HALF_ONE 1.0f16
";

/// Types nabi cannot express are skipped; a struct holding one is opaque
/// bytes; `_Float16` is `f16`. A cross target gets only clang's own
/// headers.
#[test]
fn bindgen_skips_what_nabi_cannot_express() {
    if crate::tools::test_target().is_some() {
        return;
    }
    let dir = scratch("bindgen_unsupported");
    put(dir.join("u.h"), BINDGEN_UNSUPPORTED_H).unwrap();
    put(dir.join("app/src/main.nabi"), "mod u;\n\nfn main() {}\n").unwrap();
    let t = "x86_64-unknown-linux-gnu";
    let out = bindgen(&dir, &["u.h", "--target", t, "-o", "app/src/u.nabi"]);
    assert_exit(&out, 0);
    let b = fs::read_to_string(dir.join("app/src/u.nabi")).unwrap();
    for want in [
        "// skipped typedef `vcb`: a variadic function pointer\n",
        "// skipped fn `add128`: type `__int128` has no nabi form\n",
        "// skipped fn `uadd128`: type `unsigned __int128` has no nabi form\n",
        "    pub fn half(x: f16) -> f16;\n",
        "// skipped fn `ld`: type `long double` has no nabi form\n",
        "// `wide` as bytes (field `x`: type `long double` has no nabi form)\n\
         #[repr(C, align(16))]\npub struct wide {\n    pub _opaque: [u8; 32];\n}\n",
        "#[repr(C)]\npub struct outer {\n    pub i: inner;\n    pub k: i32;\n}\n",
        "// skipped fn `take_wide`: `wide` is passed by value but written as bytes (field `x`: \
         type `long double` has no nabi form)\n",
        "// skipped fn `give_outer`: `inner` is passed by value but written as bytes (field `v`: \
         type `__int128` has no nabi form)\n",
        "// skipped typedef `wide_cb`: `wide` is passed by value but written as bytes",
        "// `has_vcb` as bytes (field `f`: `vcb` is a variadic function pointer)\n",
        "// skipped fn `use_vcb`: `vcb` is a variadic function pointer\n",
        "    pub fn take_wide_ptr(w: CPtr<wide>);\n",
        "    pub fn take_plain(p: plain, n: usize);\n",
    ] {
        assert!(b.contains(want), "lacks {want:?}:\n{b}");
    }
    assert!(!b.contains("LD_ONE") && !b.contains("HALF_ONE"), "{b}");
    assert_exit(
        &nabi_in(None, &dir.join("app"), &["-q", "check", "--target", t]),
        0,
    );

    // wasm32-wasip2 is never this machine: its headers are given.
    put(dir.join("s.h"), "#include <stdio.h>\nint f(FILE *f);\n").unwrap();
    let out = bindgen(&dir, &["s.h", "--target", "wasm32-wasip2"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains(
            "'stdio.h' file not found\nnote: wasm32-wasip2 is not this machine's platform, so its \
             system headers are not used; give the target's after `--` (`-isystem <dir>` or \
             `--sysroot <dir>`)"
        ),
        "{}",
        text(&out.stderr)
    );
    put(dir.join("inc/stdio.h"), "typedef struct _IO_FILE FILE;\n").unwrap();
    let inc = dir.join("inc");
    let args = [
        "s.h",
        "--target",
        "wasm32-wasip2",
        "--",
        "-isystem",
        inc.to_str().unwrap(),
    ];
    let out = bindgen(&dir, &args);
    assert_exit(&out, 0);
    assert!(
        text(&out.stdout).contains("    pub fn f(f: CPtr<FILE>) -> i32;\n"),
        "{}",
        text(&out.stdout)
    );
}

/// System headers bind (with what they include from their own tree)
/// into declarations nabic accepts.
#[test]
fn bindgen_binds_system_headers() {
    if crate::tools::test_target().is_some() {
        return;
    }
    // Where the host's C headers are.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        dirs.extend(std::env::split_paths(
            &std::env::var_os("INCLUDE").unwrap_or_default(),
        ));
    } else if cfg!(target_os = "macos") {
        let sdk = Command::new("xcrun")
            .arg("--show-sdk-path")
            .output()
            .unwrap();
        dirs.push(PathBuf::from(text(&sdk.stdout).trim()).join("usr/include"));
    } else {
        dirs = host_c_includes();
    }
    let dir = scratch("bindgen_sys");
    let mut bound = 0;
    for h in [
        "stdio.h", "stdlib.h", "string.h", "math.h", "time.h", "zlib.h",
    ] {
        // The C library's own header, not a wrapper that defers to it
        // (Alpine's fortify-headers come first in cc's search list).
        let Some(path) = dirs
            .iter()
            .map(|d| d.join(h))
            .find(|p| fs::read_to_string(p).is_ok_and(|c| !c.contains("#include_next")))
        else {
            continue;
        };
        let pkg = dir.join(h.replace('.', "_"));
        put(pkg.join("src/main.nabi"), "mod b;\n\nfn main() {}\n").unwrap();
        let out = pkg.join("src/b.nabi");
        let args = [path.to_str().unwrap(), "-o", out.to_str().unwrap()];
        let made = bindgen(&dir, &args);
        assert_exit(&made, 0);
        let b = fs::read_to_string(&out).unwrap();
        assert!(b.contains("extern \"C\" {"), "{h}:\n{b}");
        assert_exit(&nabi_in(None, &pkg, &["-q", "check"]), 0);
        bound += 1;
    }
    assert!(bound >= 3, "found only {bound} of the headers in {dirs:?}");
}
