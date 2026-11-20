//! C interop: calls, layouts, niches, float structs and callbacks.

#[allow(clippy::wildcard_imports)]
use crate::native::*;

/// The C compiler of the C ABI tests: `NABI_TEST_CC` (words: `gcc`,
/// `riscv64-linux-gnu-gcc`, `clang --target=..`, `cl`, `clang-cl`), else
/// `clang`, with `--target=<the target's C triple>` for a cross target.
pub(crate) struct Cc {
    words: Vec<String>,
    /// `cl` / `clang-cl` options (`/c /Fo..`).
    pub(crate) cl: bool,
    /// MSVC's own C: no GNU extensions, no `_Float16` or vectors.
    pub(crate) msvc: bool,
}

impl Cc {
    pub(crate) fn get() -> Self {
        let words: Vec<String> = match std::env::var("NABI_TEST_CC") {
            Ok(c) if !c.trim().is_empty() => c.split_whitespace().map(str::to_owned).collect(),
            _ if crate::common::test_target().is_none() && cfg!(unix) => vec!["clang".to_owned()],
            _ => vec![
                "clang".to_owned(),
                format!("--target={}", crate::common::spec().c_triple()),
            ],
        };
        let name = Path::new(&words[0])
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        Self {
            cl: name == "cl" || name == "clang-cl",
            msvc: name == "cl",
            words,
        }
    }

    /// It is gcc (`gcc`, a cross `<triple>-gcc`).
    pub(crate) fn gcc(&self) -> bool {
        Path::new(&self.words[0])
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == "gcc" || n.ends_with("-gcc") || n.starts_with("gcc-"))
    }

    /// Its name, for messages.
    pub(crate) fn name(&self) -> String {
        self.words.join(" ")
    }

    /// `src` compiled freestanding into the object `obj`.
    pub(crate) fn compile(&self, src: &Path, obj: &Path) -> Output {
        let mut c = Command::new(&self.words[0]);
        c.args(&self.words[1..]);
        if self.cl {
            // No stack cookie or default library: the C is freestanding.
            // `/Od`: optimized, MSVC 14.44 x64 gave a few checksums that
            // change with the object's path, not with the convention.
            c.args(["/nologo", "/Od", "/c", "/std:c11", "/GS-", "/Zl"])
                .arg(format!("/Fo{}", obj.display()));
        } else {
            // nabic links ELF executables as PIEs, and clang for FreeBSD
            // makes position-dependent code unless asked.
            if crate::common::spec().object_format == nabic_target::ObjectFormat::Elf {
                c.arg("-fPIC");
            }
            c.args(["-O1", "-ffreestanding", "-c", "-o"]).arg(obj);
        }
        c.arg(src).output().unwrap()
    }

    /// It compiles `code`.
    pub(crate) fn accepts(&self, dir: &Path, code: &str) -> bool {
        let src = dir.join("probe.c");
        fs::write(&src, code).unwrap();
        self.compile(&src, &dir.join("probe.o")).status.success()
    }
}

/// The program of `main.nabi` (built with `extra`) and `side.c` in
/// `dir`, then its run. On a Unix host the C compiler (`Cc`) links
/// nabic's object, the C and the runtime. Otherwise (`NABI_TEST_TARGET`,
/// a Windows host) it compiles the C freestanding into the `side`
/// archive, which nabic links; the program runs under
/// `NABI_TEST_RUNNER`.
pub(crate) fn c_program(env: &Env, dir: &Path, extra: &[&str]) -> Output {
    let main = dir.join("main.nabi");
    let exe = dir.join(crate::common::exe_name("out"));
    let cc = Cc::get();
    if crate::common::test_target().is_none() && cfg!(unix) {
        let obj = dir.join("main.o");
        let mut args = extra.to_vec();
        args.extend(["--emit=obj", "-o", obj.to_str().unwrap()]);
        ok(&build(env, &main, &args));
        ok(&Command::new(&cc.words[0])
            .args(&cc.words[1..])
            .arg(&obj)
            .arg(dir.join("side.c"))
            .arg(&env.nabir)
            .args(["-lm", "-lpthread", "-o"])
            .arg(&exe)
            .output()
            .unwrap());
    } else {
        let obj = dir.join("side.o");
        ok(&cc.compile(&dir.join("side.c"), &obj));
        let ar = nabild::archive::write(
            nabild::archive::Kind::Gnu,
            &[("side.o".to_owned(), fs::read(&obj).unwrap())],
        );
        let t = crate::common::spec();
        let lib = format!("{}side{}", t.staticlib_prefix, t.staticlib_suffix);
        fs::write(dir.join(lib), ar).unwrap();
        let mut args = extra.to_vec();
        args.extend([
            "-L",
            dir.to_str().unwrap(),
            "-l",
            "static=side",
            "-o",
            exe.to_str().unwrap(),
        ]);
        ok(&build(env, &main, &args));
    }
    crate::common::run_command(&exe).output().unwrap()
}

pub(crate) const C_SIDE: &str = r"
#include <stdarg.h>
typedef struct { float x, y, z; } V3;
typedef struct { double a, b, c, d; } D4;
typedef struct { long long a, b, c; } Big;
typedef struct { signed char a; short b; } Small;
typedef struct { long long a; double b; } Mix;
typedef struct { int a, b, c; } I3;
V3 v3_scale(V3 v, float k) { V3 r = { v.x * k, v.y * k, v.z * k }; return r; }
double d4_sum(D4 d) { return d.a + d.b + d.c + d.d; }
Big big_make(long long a) { Big b = { a, a * 2, a * 3 }; return b; }
long long big_sum(Big b) { return b.a + b.b + b.c; }
Small small_inc(Small s) { s.a += 1; s.b += 1; return s; }
Mix mix_twice(Mix m) { m.a *= 2; m.b *= 2; return m; }
int i3_sum(I3 s) { return s.a + s.b + s.c; }
long long many(long long a, long long b, long long c, long long d, long long e, I3 s, long long f) {
    return a + b + c + d + e + s.a + s.b + s.c + f;
}
int sx(signed char c, unsigned short u) { return c * 100000 + u; }
long long vsum(int n, ...) {
    va_list ap; va_start(ap, n); long long t = 0;
    for (int i = 0; i < n; i++) t += va_arg(ap, long long);
    va_end(ap); return t;
}
typedef struct { int q, r; } DT;
DT dt_div(int n, int d) { DT x = { n / d, n % d }; return x; }
int via(DT (*f)(int, int)) { DT d = f(9, 4); return d.q * 10 + d.r; }
double vmix(int n, ...) {
    va_list ap; va_start(ap, n); double t = 0;
    for (int i = 0; i < n; i++) { t += va_arg(ap, int); t += va_arg(ap, double); }
    va_end(ap); return t;
}
int cb_v3(V3 (*f)(V3, float)) { V3 v = { 1, 2, 3 }; V3 r = f(v, 3.0f); return (int)(r.x + r.y + r.z); }
long long cb_big(Big (*f)(Big)) { Big b = { 1, 2, 3 }; Big r = f(b); return r.a * 100 + r.b * 10 + r.c; }
double cb_d4(double (*f)(D4)) { D4 d = { 1, 2, 3, 4 }; return f(d); }
Small cb_small(Small (*f)(Small)) { Small s = { 4, 5 }; return f(s); }
Mix nabi_mix(Mix m);
double call_nabi_mix(void) { Mix m = { 3, 0.5 }; Mix r = nabi_mix(m); return (double)(int)r.a + r.b; }
Big rev_c(Big b) { Big r = { b.c, b.b, b.a }; return r; }
V3 (*get_v3_scale(void))(V3, float) { return v3_scale; }
Big (*get_rev(void))(Big) { return rev_c; }
long long cb_big2(Big (*f)(Big)) { return cb_big(f); }
signed char neg8(signed char c) { return -c; }
signed char nabi_neg8(signed char c);
unsigned short nabi_dbl16(unsigned short u);
int call_narrow(void) { return nabi_neg8(-7) * 100000 + nabi_dbl16(40000); }
char cid(char c) { return c; }
typedef struct { float x; double y; } FD;
typedef struct { float f; int i; } FI;
FD fd_mul(FD v, int k) { FD r = { v.x * k, v.y * k }; return r; }
FI fi_inc(FI v) { v.f += 1; v.i += 1; return v; }
int ult(unsigned a, unsigned b) { return a < b; }
";

pub(crate) const NABI_SIDE: &str = r#"
struct V3 { x: f32; y: f32; z: f32; }
struct D4 { a: f64; b: f64; c: f64; d: f64; }
struct Big { a: i64; b: i64; c: i64; }
struct Small { a: i8; b: i16; }
struct Mix { a: i64; b: f64; }
struct I3 { a: i32; b: i32; c: i32; }
struct DT { q: i32; r: i32; }
struct FD { x: f32; y: f64; }
struct FI { f: f32; i: i32; }

extern "C" {
    fn v3_scale(v: V3, k: f32) -> V3;
    fn d4_sum(d: D4) -> f64;
    fn big_make(a: i64) -> Big;
    fn big_sum(b: Big) -> i64;
    fn small_inc(s: Small) -> Small;
    fn mix_twice(m: Mix) -> Mix;
    fn i3_sum(s: I3) -> i32;
    fn many(a: i64, b: i64, c: i64, d: i64, e: i64, s: I3, f: i64) -> i64;
    fn sx(c: i8, u: u16) -> i32;
    fn vsum(n: i32, ...) -> i64;
    fn vmix(n: i32, ...) -> f64;
    fn dt_div(n: i32, d: i32) -> DT;
    fn via(f: unsafe fn(i32, i32) -> DT) -> i32;
    fn printf(fmt: str, ...) -> i32;
    fn cb_v3(f: fn(V3, f32) -> V3) -> i32;
    fn cb_big(f: fn(Big) -> Big) -> i64;
    fn cb_d4(f: fn(D4) -> f64) -> f64;
    fn cb_small(f: fn(Small) -> Small) -> Small;
    fn call_nabi_mix() -> f64;
    // C function pointers as values.
    fn get_v3_scale() -> extern "C" fn(V3, f32) -> V3;
    fn get_rev() -> extern "C" fn(Big) -> Big;
    fn cb_big2(f: extern "C" fn(Big) -> Big) -> i64;
    fn neg8(c: i8) -> i8;
    fn call_narrow() -> i32;
    fn cid(c: c_char) -> c_char;
    fn fd_mul(v: FD, k: i32) -> FD;
    fn fi_inc(v: FI) -> FI;
    fn ult(a: u32, b: u32) -> i32;
}

// C calls these: aggregates the C way both ways.
extern "C" fn scale_v3(v: V3, k: f32) -> V3 { V3 { x: v.x * k, y: v.y * k, z: v.z * k } }
extern "C" fn rev_big(b: Big) -> Big { Big { a: b.c, b: b.b, c: b.a } }
extern "C" fn sum_d4(d: D4) -> f64 { d.a + d.b + d.c + d.d }
extern "C" fn neg_small(s: Small) -> Small { Small { a: -s.a, b: -s.b } }
#[export_name("nabi_mix")]
extern "C" fn mix_nabi(m: Mix) -> Mix { Mix { a: m.a + 1, b: m.b * 2.0 } }
// Narrow results come back extended where the platform ABI says so.
#[export_name("nabi_neg8")]
extern "C" fn neg8_nabi(c: i8) -> i8 { -c }
#[export_name("nabi_dbl16")]
extern "C" fn dbl16_nabi(u: u16) -> u16 { u.wrapping_mul(2) }

fn main() {
    unsafe {
        let v = v3_scale(V3 { x: 1.0, y: 2.0, z: 3.0 }, 2.0);
        println(f"{v.x} {v.y} {v.z}");
        println(f"{d4_sum(D4 { a: 1.0, b: 2.0, c: 3.0, d: 4.5 })}");
        let b = big_make(5);
        println(f"{b.a} {b.b} {b.c} {big_sum(b)}");
        let s = small_inc(Small { a: -3, b: 300 });
        println(f"{s.a} {s.b}");
        let m = mix_twice(Mix { a: 21, b: 1.25 });
        println(f"{m.a} {m.b}");
        println(f"{i3_sum(I3 { a: 1, b: 2, c: 3 })}");
        println(f"{many(1, 2, 3, 4, 5, I3 { a: 6, b: 7, c: 8 }, 9)}");
        let c: i8 = -5;
        let u: u16 = 65535;
        println(f"{sx(c, u)}");
        let x: i64 = 40;
        println(f"{vsum(3, x, x, 2 as i64)}");
        let small: i8 = -2;
        let half: f32 = 0.5;
        println(f"{vmix(2, small, half, true, 1.25)}");
        // A C function handed back to C as a pointer keeps its C type.
        println(f"{via(dt_div)}");
        let n = cb_small(neg_small);
        println(f"{cb_v3(scale_v3)} {cb_big(rev_big)} {cb_d4(sum_d4) as i64} {call_nabi_mix() as i64} {n.a} {n.b}");
    }
    // From nabi they are nabi functions.
    let r = rev_big(Big { a: 7, b: 8, c: 9 });
    println(f"{r.a}{r.b}{r.c}");
    // `extern "C" fn` values: received from C, made from a nabi `extern
    // "C" fn` (its C entry), called with the C ABI, handed back to C.
    let f = unsafe { get_v3_scale() };
    let w = f(V3 { x: 1.0, y: 2.0, z: 3.0 }, 2.0);
    let g: extern "C" fn(Big) -> Big = unsafe { get_rev() };
    let rb = g(Big { a: 1, b: 2, c: 3 });
    let h: extern "C" fn(Big) -> Big = rev_big;
    let rh = h(Big { a: 4, b: 5, c: 6 });
    let k: extern "C" fn(Small) -> Small | none = neg_small;
    let ks = match k { none => Small { a: 0, b: 0 }, p: extern "C" fn(Small) -> Small => p(Small { a: 1, b: 2 }) };
    println(f"{w.x} {w.y} {w.z} {rb.a}{rb.b}{rb.c} {rh.a}{rh.b}{rh.c} {ks.a} {ks.b} {unsafe { cb_big2(h) }}");
    println(f"{unsafe { neg8(5) }} {unsafe { call_narrow() }}");
    println(f"{unsafe { cid(c'A') } as i32}");
    let fd = unsafe { fd_mul(FD { x: 1.5, y: 2.25 }, -2) };
    let fi = unsafe { fi_inc(FI { f: 0.5, i: -8 }) };
    let big: u32 = 4000000000;
    println(f"{fd.x} {fd.y} {fi.f} {fi.i} {unsafe { ult(5, big) }}{unsafe { ult(big, 5) }}");
}
"#;

/// Aggregates by value both ways, HFAs, `sret`, register exhaustion,
/// caller-extended narrow ints and variadic promotions, against clang.
pub(crate) const NICHE_C: &str = r"
#include <stddef.h>
typedef long long (*op)(long long);
typedef struct { op f; long long x; } Cb;
long long twice(long long x) { return 2 * x; }
op get_op(int some) { return some ? twice : NULL; }
long long call_or(op f, long long x, long long d) { return f ? f(x) : d; }
long long run_cb(Cb c) { return c.f ? c.f(c.x) : -1; }
Cb make_cb(int some) { Cb c = { some ? twice : NULL, 21 }; return c; }
op slot = 0;
long long call_slot(long long x) { return slot ? slot(x) : -1; }
int nz_or(int v, int d) { return v ? v : d; }
int nz_make(int some) { return some ? 7 : 0; }
long long g = 77;
long long *get_g(int some) { return some ? &g : NULL; }
long long ptr_read_or(long long *p, long long d) { return p ? *p : d; }
";

pub(crate) const NICHE_NABI: &str = r#"
use bare::ptr::Ptr;
use core::num::niche::NonZero;

struct Cb { f: extern "C" fn(i64) -> i64 | none; x: i64; }

extern "C" {
    fn twice(x: i64) -> i64;
    fn get_op(some: i32) -> extern "C" fn(i64) -> i64 | none;
    fn call_or(f: extern "C" fn(i64) -> i64 | none, x: i64, d: i64) -> i64;
    fn run_cb(c: Cb) -> i64;
    fn make_cb(some: i32) -> Cb;
    fn call_slot(x: i64) -> i64;
    static mut slot: extern "C" fn(i64) -> i64 | none;
    fn nz_or(v: NonZero<i32> | none, d: i32) -> i32;
    fn nz_make(some: i32) -> NonZero<i32> | none;
    fn get_g(some: i32) -> Ptr<i64> | none;
    fn ptr_read_or(p: Ptr<i64> | none, d: i64) -> i64;
}

extern "C" fn neg(x: i64) -> i64 { -x }

fn show(f: extern "C" fn(i64) -> i64 | none) -> str {
    match f { none => "none", g: extern "C" fn(i64) -> i64 => f"{g(5)}" }
}

fn widen(f: extern "C" fn(i64) -> i64 | none) -> extern "C" fn(i64) -> i64 | none | i32 { f }

fn back(v: extern "C" fn(i64) -> i64 | none | i32) -> str {
    match v { g: extern "C" fn(i64) -> i64 => f"fn {g(1)}", none => "none", i: i32 => f"i32 {i}" }
}

fn main() {
    let a = unsafe { get_op(1) };
    let z = unsafe { get_op(0) };
    println(f"{a is none} {z is none} {show(a)} {show(z)}");
    if let g: extern "C" fn(i64) -> i64 = a { println(f"if-let {g(21)}"); }
    if let g: extern "C" fn(i64) -> i64 = z { println(f"bad {g(1)}"); } else { println("if-let none"); }
    let n: extern "C" fn(i64) -> i64 | none = none;
    let p: extern "C" fn(i64) -> i64 | none = neg;
    println(f"{unsafe { call_or(n, 4, 99) }} {unsafe { call_or(p, 4, 99) }} {unsafe { call_or(a, 4, 99) }}");
    let xs: List<extern "C" fn(i64) -> i64 | none> = List::new();
    xs.push(a);
    xs.push(z);
    xs.push(p);
    for f in xs { print(f"{show(f)},"); }
    println("");
    let c = Cb { f: p, x: 7 };
    let d = Cb { f: none, x: 7 };
    let m = unsafe { make_cb(1) };
    let k = unsafe { make_cb(0) };
    println(f"{unsafe { run_cb(c) }} {unsafe { run_cb(d) }} {show(m.f)} {m.x} {show(k.f)} {unsafe { run_cb(m) }}");
    let wi: extern "C" fn(i64) -> i64 | none | i32 = 3;
    println(f"{back(widen(a))} {back(widen(z))} {back(wi)}");
    let narrow: extern "C" fn(i64) -> i64 | none = match widen(a) { i: i32 => none, g: extern "C" fn(i64) -> i64 | none => g };
    println(f"{show(narrow)}");
    let dflt: extern "C" fn(i64) -> i64 = neg;
    let h = z ?? dflt;
    let h2 = a ?? dflt;
    println(f"{h(3)} {h2(3)}");
    unsafe {
        println(f"{show(slot)} {call_slot(50)}");
        slot = neg;
        println(f"{show(slot)} {call_slot(50)}");
        slot = none;
        println(f"{slot is none} {call_slot(50)}");
        let nz: NonZero<i32> | none = NonZero::new(5);
        let nn: NonZero<i32> | none = none;
        let mk = nz_make(1) ?? NonZero::new(1) ?? panic("zero");
        println(f"{nz_or(nz, 1)} {nz_or(nn, 1)} {mk} {nz_make(0) is none}");
        let p = get_g(1);
        let rd: i64 = match p { q: Ptr<i64> => q.read(), none => -1 };
        println(f"{ptr_read_or(p, -1)} {ptr_read_or(get_g(0), -1)} {rd} {get_g(0) is none}");
    }
}
"#;

/// Null niches across the C ABI: nullable fn pointers, `NonZero<i32> |
/// none` as `int`, `Ptr<T> | none` as `T*`.
#[test]
fn c_fn_ptr_null_niche() {
    let env = setup();
    let dir = scratch("cniche");
    fs::write(dir.join("side.c"), NICHE_C).unwrap();
    fs::write(dir.join("main.nabi"), NICHE_NABI).unwrap();
    let bare = env.workspace.join("library/bare/src/lib.nabi");
    let run = c_program(&env, &dir, &["--bootstrap", bare.to_str().unwrap()]);
    ok(&run);
    assert_eq!(
        text(&run.stdout),
        "false true 10 none\nif-let 42\nif-let none\n99 -4 8\n10,none,-5,\n\
         -7 -1 10 21 none 42\nfn 2 none i32 3\n10\n-3 6\nnone -1\n-5 -50\ntrue -1\n\
         5 1 7 true\n77 -1 77 true\n"
    );
}

/// Ten arguments, `str`s past the last argument register. Runtime ABI:
/// a `str` is a by-value `(ptr, len)` struct, never split
/// between the last register and the stack. C ABI: a `char*`.
pub(crate) const PAIRS_C: &str = r"
#include <stddef.h>
static size_t slen(const char *s) { size_t n = 0; while (s[n]) n++; return n; }
typedef struct { const char *p; size_t n; } S;
static long long h(S s) { return s.n ? (long long)s.n * 1000 + s.p[0] : 0; }
long long rt10(S a, long long b, long long c, long long d, S e, S f, long long g, S h1, long long i, S j) {
    return h(a) + b + c * 10 + d * 100 + h(e) * 2 + h(f) * 3 + g * 7 + h(h1) * 5 + i * 11 + h(j) * 13;
}
S rt_pick(long long a, long long b, long long c, long long d, long long e, long long f, long long g, S s, long long i) {
    S r = { s.p + (a + b + c + d + e + f + g + i - 35), s.n - 1 };
    return r;
}
static long long hc(const char *s) { return (long long)slen(s) * 1000 + s[0]; }
long long c10(const char *a, long long b, long long c, long long d, const char *e, const char *f, long long g,
         const char *h1, long long i, const char *j) {
    return hc(a) + b + c * 10 + d * 100 + hc(e) * 2 + hc(f) * 3 + g * 7 + hc(h1) * 5 + i * 11 + hc(j) * 13;
}
";

pub(crate) const PAIRS_NABI: &str = r#"
extern "nabi" {
    fn rt10(a: str, b: i64, c: i64, d: i64, e: str, f: str, g: i64, h: str, i: i64, j: str) -> i64;
    fn rt_pick(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, s: str, i: i64) -> str;
}

extern "C" {
    fn c10(a: str, b: i64, c: i64, d: i64, e: str, f: str, g: i64, h: str, i: i64, j: str) -> i64;
}

fn main() {
    let x = unsafe { rt10("a", 1, 2, 3, "bc", "def", 4, "ghij", 5, "klmno") };
    let y = unsafe { c10("a", 1, 2, 3, "bc", "def", 4, "ghij", 5, "klmno") };
    let s = unsafe { rt_pick(1, 2, 3, 4, 5, 6, 7, "xyzw", 8) };
    println(f"{x} {y} {s}");
}
"#;

#[test]
fn ten_argument_calls_with_strs() {
    let env = setup();
    let dir = scratch("pairs");
    fs::write(dir.join("side.c"), PAIRS_C).unwrap();
    fs::write(dir.join("main.nabi"), PAIRS_NABI).unwrap();
    let run = c_program(&env, &dir, &[]);
    ok(&run);
    // h(s) = len * 1000 + first byte.
    let h = |s: &str| s.len() as i64 * 1000 + i64::from(s.as_bytes()[0]);
    let want = h("a")
        + 1
        + 20
        + 300
        + h("bc") * 2
        + h("def") * 3
        + 28
        + h("ghij") * 5
        + 55
        + h("klmno") * 13;
    assert_eq!(text(&run.stdout), format!("{want} {want} yzw\n"));
}

/// `#[repr(..)]` layouts against clang's.
pub(crate) const REPR_C: &str = r"
#include <stddef.h>
typedef struct { char a; int b; char c; } C1;
typedef struct __attribute__((packed)) { char a; int b; short c; } P;
typedef struct { char x; P p; char y; } HasP;
typedef struct __attribute__((aligned(16))) { char a; } A;
typedef struct { char x; A a; char y; } Outer;
long long layout(int i) {
    static const long long t[] = {
        sizeof(C1), _Alignof(C1), offsetof(C1, b), offsetof(C1, c),
        sizeof(P), _Alignof(P), offsetof(P, b), offsetof(P, c),
        sizeof(HasP), offsetof(HasP, p), offsetof(HasP, y),
        sizeof(A), _Alignof(A), sizeof(Outer), offsetof(Outer, a), offsetof(Outer, y),
    };
    return t[i];
}
long long c1_sum(C1 s) { return s.a * 100 + s.b * 10 + s.c; }
long long p_sum(P p) { return p.a * 100 + p.b * 10 + p.c; }
long long has_p(HasP h) { return h.x * 1000 + h.p.b * 10 + h.y; }
long long outer(Outer o) { return o.x * 100 + o.a.a * 10 + o.y; }
P p_make(int b) { P p = { 1, b, 3 }; return p; }
double twice(double d) { return d * 2; }
";

pub(crate) const REPR_NABI: &str = r#"
use bare::mem::{align_of, size_of};

#[repr(C)]
struct C1 { a: i8; b: i32; c: i8; }
#[repr(packed)]
struct P { a: i8; b: i32; c: i16; }
struct HasP { x: i8; p: P; y: i8; }
#[repr(align(16))]
struct A { a: i8; }
struct Outer { x: i8; a: A; y: i8; }
#[repr(transparent)]
struct T { v: f64; }

extern "C" {
    fn layout(i: i32) -> i64;
    fn c1_sum(s: C1) -> i64;
    fn p_sum(p: P) -> i64;
    fn has_p(h: HasP) -> i64;
    fn outer(o: Outer) -> i64;
    fn p_make(b: i32) -> P;
    fn twice(t: T) -> T;
}

fn main() {
    let c = unsafe { [layout(0), layout(1), layout(4), layout(5), layout(8), layout(11), layout(12), layout(13)] };
    println(f"{c[0]} {c[1]} {c[2]} {c[3]} {c[4]} {c[5]} {c[6]} {c[7]}");
    println(f"{size_of::<C1>()} {align_of::<C1>()} {size_of::<P>()} {align_of::<P>()} {size_of::<HasP>()} {size_of::<A>()} {align_of::<A>()} {size_of::<Outer>()}");
    let p = unsafe { p_make(7) };
    unsafe {
        println(f"{c1_sum(C1 { a: 1, b: 2, c: 3 })} {p_sum(P { a: 4, b: 5, c: 6 })} {has_p(HasP { x: 1, p: P { a: 0, b: 2, c: 0 }, y: 3 })} {outer(Outer { x: 7, a: A { a: 8 }, y: 9 })}");
        println(f"{p.a} {p.b} {p.c} {twice(T { v: 1.25 }).v}");
    }
}
"#;

#[test]
fn repr_layouts_match_clang() {
    // MSVC lays a packed struct out otherwise, `Outer` cannot go by
    // value to i686 Windows cdecl C (an error), and MSVC's C has no GNU
    // attributes.
    let t = crate::common::spec();
    if (t.env == "msvc" && t.arch == "x86") || Cc::get().msvc {
        return;
    }
    let env = setup();
    let dir = scratch("repr");
    fs::write(dir.join("side.c"), REPR_C).unwrap();
    fs::write(dir.join("main.nabi"), REPR_NABI).unwrap();
    let bare = env.workspace.join("library/bare/src/lib.nabi");
    let run = c_program(&env, &dir, &["--bootstrap", bare.to_str().unwrap()]);
    ok(&run);
    let out = text(&run.stdout);
    let lines: Vec<&str> = out.lines().collect();
    // clang's sizes / alignments, then nabi's: the same.
    assert_eq!(lines[0], "12 4 7 1 9 16 16 48", "{out}");
    assert_eq!(lines[1], lines[0], "{out}");
    assert_eq!(lines[2], "123 456 1023 789", "{out}");
    assert_eq!(lines[3], "1 7 3 2.5", "{out}");
}

#[test]
fn c_abi_matches_clang() {
    let env = setup();
    let dir = scratch("cabi");
    fs::write(dir.join("side.c"), C_SIDE).unwrap();
    fs::write(dir.join("main.nabi"), NABI_SIDE).unwrap();
    let run = c_program(&env, &dir, &[]);
    ok(&run);
    assert_eq!(
        text(&run.stdout),
        "2 4 6\n10.5\n5 10 15 30\n-2 301\n42 2.5\n6\n45\n-434465\n82\n0.75\n21\n18 321 10 5 -4 -5\n987\n2 4 6 321 654 -1 -2 321\n-5 714464\n65\n-3 -4.5 1.5 -7 10\n"
    );
}

/// Small float structs both ways, as the RISC-V LP64D float convention
/// flattens them (one float, two floats, a float and an integer; nested,
/// in arrays, padded, packed) and where registers run out; on other
/// targets the same C ABI checks.
pub(crate) const FP_C: &str = r"
typedef struct { float a; } F1;
typedef struct { double a; float b; } DF;
typedef struct { float a; int b; } FI;
typedef struct { signed char a; double b; } CD;
typedef struct { long long a; float b; } LF;
typedef struct { struct { float x; } in; float y[1]; } NF;
typedef struct { double v[2]; } DA;
typedef struct __attribute__((packed)) { signed char a; float b; } PCF;
typedef struct __attribute__((packed)) { float a; double b; } PFD;
typedef struct __attribute__((aligned(16))) { double d; } A16;
typedef struct { float a; A16 b; } PAD;
typedef struct { float a, b; } FF;
typedef struct { float a, b, c; } F3;
F1 f1(F1 v) { v.a += 1; return v; }
DF df(DF v) { v.a *= 2; v.b += 0.5f; return v; }
FI fi(FI v, int k) { v.a *= k; v.b += k; return v; }
CD cd(CD v) { v.a += 1; v.b *= 3; return v; }
LF lf(LF v) { v.a = -v.a; v.b *= 2; return v; }
NF nf(NF v) { NF r = { { v.y[0] }, { v.in.x } }; return r; }
DA da(DA v) { DA r = { { v.v[1], v.v[0] } }; return r; }
PCF pcf(PCF v) { v.a += 2; v.b *= -1; return v; }
PFD pfd(PFD v) { v.a += 1; v.b += 1; return v; }
PAD pad(PAD v) { v.a *= 2; v.b.d *= 2; return v; }
F3 f3(F3 v) { F3 r = { v.c, v.b, v.a }; return r; }
double ff7(double a, double b, double c, double d, double e, double f, double g, FF s, float t) {
    return a + b + c + d + e + f + g + s.a * 10 + s.b * 100 + t * 1000;
}
long long fi8(long long a, long long b, long long c, long long d, long long e, long long f,
              long long g, long long h, FI s, float t) {
    return a + b + c + d + e + f + g + h + (long long)(s.a * 10) + s.b * 100 + (long long)(t * 1000);
}
double mixed(int i, FI a, double d, CD b, float f, DF c, LF e) {
    return i + a.a + a.b * 10 + d * 100 + b.a * 1000 + b.b * 10000 + f * 1e5 + c.a * 1e6 + c.b * 1e7
        + e.a * 1e8 + e.b * 1e9;
}
F1 nabi_f1(F1);
DF nabi_df(DF);
FI nabi_fi(FI, int);
CD nabi_cd(CD);
LF nabi_lf(LF);
NF nabi_nf(NF);
DA nabi_da(DA);
PCF nabi_pcf(PCF);
PFD nabi_pfd(PFD);
PAD nabi_pad(PAD);
double nabi_ff7(double, double, double, double, double, double, double, FF, float);
long long nabi_fi8(long long, long long, long long, long long, long long, long long,
                   long long, long long, FI, float);
double nabi_mixed(int, FI, double, CD, float, DF, LF);
double c_calls(void) {
    F1 a = nabi_f1((F1){ 1.5f });
    DF b = nabi_df((DF){ 2.5, 3.0f });
    FI c = nabi_fi((FI){ 1.5f, 7 }, 3);
    CD d = nabi_cd((CD){ -4, 0.25 });
    LF e = nabi_lf((LF){ 9, 0.75f });
    NF f = nabi_nf((NF){ { 1.0f }, { 2.0f } });
    DA g = nabi_da((DA){ { 3.0, 4.0 } });
    PCF h = nabi_pcf((PCF){ 5, 6.5f });
    PFD i = nabi_pfd((PFD){ 0.5f, 7.0 });
    PAD j = nabi_pad((PAD){ 1.25f, { 2.5 } });
    double k = nabi_ff7(1, 2, 3, 4, 5, 6, 7, (FF){ 0.5f, 0.25f }, 0.125f);
    long long l = nabi_fi8(1, 2, 3, 4, 5, 6, 7, 8, (FI){ 2.5f, 3 }, 0.5f);
    double m = nabi_mixed(1, (FI){ 2, 3 }, 4, (CD){ 5, 6 }, 7, (DF){ 8, 9 }, (LF){ 1, 2 });
    return a.a + b.a + b.b + c.a + c.b + d.a + d.b + e.a + e.b + f.in.x + f.y[0] * 10 + g.v[0]
        + g.v[1] * 10 + h.a + h.b + i.a + i.b + j.a + j.b.d + k + l + m;
}
";

pub(crate) const FP_NABI: &str = r#"
struct F1 { a: f32; }
struct DF { a: f64; b: f32; }
struct FI { a: f32; b: i32; }
struct CD { a: i8; b: f64; }
struct LF { a: i64; b: f32; }
struct In { x: f32; }
struct NF { inner: In; y: [f32; 1]; }
struct DA { v: [f64; 2]; }
#[repr(packed)]
struct PCF { a: i8; b: f32; }
#[repr(packed)]
struct PFD { a: f32; b: f64; }
#[repr(align(16))]
struct A16 { d: f64; }
struct PAD { a: f32; b: A16; }
struct FF { a: f32; b: f32; }
struct F3 { a: f32; b: f32; c: f32; }

extern "C" {
    fn f1(v: F1) -> F1;
    fn df(v: DF) -> DF;
    fn fi(v: FI, k: i32) -> FI;
    fn cd(v: CD) -> CD;
    fn lf(v: LF) -> LF;
    fn nf(v: NF) -> NF;
    fn da(v: DA) -> DA;
    fn pcf(v: PCF) -> PCF;
    fn pfd(v: PFD) -> PFD;
    fn pad(v: PAD) -> PAD;
    fn f3(v: F3) -> F3;
    fn ff7(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, s: FF, t: f32) -> f64;
    fn fi8(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64, s: FI, t: f32) -> i64;
    fn mixed(i: i32, a: FI, d: f64, b: CD, f: f32, c: DF, e: LF) -> f64;
    fn c_calls() -> f64;
}

#[export_name("nabi_f1")]
extern "C" fn n_f1(v: F1) -> F1 { F1 { a: v.a + 1.0 } }
#[export_name("nabi_df")]
extern "C" fn n_df(v: DF) -> DF { DF { a: v.a * 2.0, b: v.b + 0.5 } }
#[export_name("nabi_fi")]
extern "C" fn n_fi(v: FI, k: i32) -> FI { FI { a: v.a * k as f32, b: v.b + k } }
#[export_name("nabi_cd")]
extern "C" fn n_cd(v: CD) -> CD { CD { a: v.a + 1, b: v.b * 3.0 } }
#[export_name("nabi_lf")]
extern "C" fn n_lf(v: LF) -> LF { LF { a: -v.a, b: v.b * 2.0 } }
#[export_name("nabi_nf")]
extern "C" fn n_nf(v: NF) -> NF { NF { inner: In { x: v.y[0] }, y: [v.inner.x] } }
#[export_name("nabi_da")]
extern "C" fn n_da(v: DA) -> DA { DA { v: [v.v[1], v.v[0]] } }
#[export_name("nabi_pcf")]
extern "C" fn n_pcf(v: PCF) -> PCF { PCF { a: v.a + 2, b: -v.b } }
#[export_name("nabi_pfd")]
extern "C" fn n_pfd(v: PFD) -> PFD { PFD { a: v.a + 1.0, b: v.b + 1.0 } }
#[export_name("nabi_pad")]
extern "C" fn n_pad(v: PAD) -> PAD { PAD { a: v.a * 2.0, b: A16 { d: v.b.d * 2.0 } } }
#[export_name("nabi_ff7")]
extern "C" fn n_ff7(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, s: FF, t: f32) -> f64 {
    a + b + c + d + e + f + g + s.a as f64 * 10.0 + s.b as f64 * 100.0 + t as f64 * 1000.0
}
#[export_name("nabi_fi8")]
extern "C" fn n_fi8(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64, s: FI, t: f32) -> i64 {
    a + b + c + d + e + f + g + h + (s.a * 10.0) as i64 + s.b as i64 * 100 + (t * 1000.0) as i64
}
#[export_name("nabi_mixed")]
extern "C" fn n_mixed(i: i32, a: FI, d: f64, b: CD, f: f32, c: DF, e: LF) -> f64 {
    i as f64 + a.a as f64 + a.b as f64 * 10.0 + d * 100.0 + b.a as f64 * 1000.0 + b.b * 10000.0
        + f as f64 * 100000.0 + c.a * 1000000.0 + c.b as f64 * 10000000.0
        + e.a as f64 * 100000000.0 + e.b as f64 * 1000000000.0
}

fn main() {
    unsafe {
        let a = f1(F1 { a: 1.5 });
        let b = df(DF { a: 2.5, b: 3.0 });
        let c = fi(FI { a: 1.5, b: 7 }, 3);
        let d = cd(CD { a: -4, b: 0.25 });
        let e = lf(LF { a: 9, b: 0.75 });
        println(f"{a.a} {b.a} {b.b} {c.a} {c.b} {d.a} {d.b} {e.a} {e.b}");
        let f = nf(NF { inner: In { x: 1.0 }, y: [2.0] });
        let g = da(DA { v: [3.0, 4.0] });
        let h = pcf(PCF { a: 5, b: 6.5 });
        let i = pfd(PFD { a: 0.5, b: 7.0 });
        let j = pad(PAD { a: 1.25, b: A16 { d: 2.5 } });
        let k = f3(F3 { a: 1.0, b: 2.0, c: 3.0 });
        println(f"{f.inner.x} {f.y[0]} {g.v[0]} {g.v[1]} {h.a} {h.b} {i.a} {i.b} {j.a} {j.b.d} {k.a} {k.c}");
        let x = ff7(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, FF { a: 0.5, b: 0.25 }, 0.125);
        let y = fi8(1, 2, 3, 4, 5, 6, 7, 8, FI { a: 2.5, b: 3 }, 0.5);
        let z = mixed(1, FI { a: 2.0, b: 3 }, 4.0, CD { a: 5, b: 6.0 }, 7.0, DF { a: 8.0, b: 9.0 }, LF { a: 1, b: 2.0 });
        println(f"{x} {y} {z}");
        println(f"{c_calls()}");
    }
}
"#;

#[test]
fn c_abi_float_structs() {
    // `PAD`'s 16-aligned member cannot go by value to i686 Windows cdecl
    // C (an error), and MSVC's C has no GNU attributes; the table covers
    // the rest there.
    let t = crate::common::spec();
    if (t.arch == "x86" && t.os == "windows") || Cc::get().msvc {
        return;
    }
    let env = setup();
    let dir = scratch("cfloat");
    fs::write(dir.join("side.c"), FP_C).unwrap();
    fs::write(dir.join("main.nabi"), FP_NABI).unwrap();
    let run = c_program(&env, &dir, &[]);
    ok(&run);
    assert_eq!(
        text(&run.stdout),
        "2.5 5 3.5 4.5 10 -3 0.75 -9 1.5\n\
         2 1 4 3 7 -6.5 1.5 8 2.5 5 3 1\n\
         183 861 2198765433\n\
         2198766556.25\n"
    );
}

/// An `extern "C" fn` called on threads nabi did not start: the first
/// call attaches the thread, enough allocation to collect runs, and the
/// threads leave the registry when they end (the second round collects
/// with them gone).
#[cfg(unix)]
#[test]
fn foreign_thread_callback_allocates() {
    // The host's clang links it.
    if crate::common::test_target().is_some() {
        return;
    }
    let env = setup();
    let dir = scratch("foreign_thread");
    fs::write(
        dir.join("side.c"),
        r"
#include <pthread.h>
typedef long (*cb_t)(long);
static cb_t g_cb;
static void *run(void *a) {
    long n = (long)a;
    long t = g_cb(n);
    return (void *)(t + g_cb(n));
}
long run_threads(cb_t f, long n) {
    pthread_t th[4];
    long total = 0;
    g_cb = f;
    for (int i = 0; i < 4; i++) pthread_create(&th[i], 0, run, (void *)n);
    for (int i = 0; i < 4; i++) {
        void *r;
        pthread_join(th[i], &r);
        total += (long)r;
    }
    return total;
}
",
    )
    .unwrap();
    fs::write(
        dir.join("main.nabi"),
        r#"
extern "C" {
    fn run_threads(f: fn(i64) -> i64, n: i64) -> i64;
}

extern "C" fn fill(n: i64) -> i64 {
    let xs: List<i64> = List::new();
    for i in 0..n { xs.push(i); }
    xs.len() as i64
}

fn main() -> i32 {
    let a = unsafe { run_threads(fill, 2000000) };
    let b = unsafe { run_threads(fill, 2000000) };
    let c = fill(3000000);
    if a == 16000000 && b == 16000000 && c == 3000000 { 0 } else { 1 }
}
"#,
    )
    .unwrap();
    let obj = dir.join("main.o");
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &["--emit=obj", "-o", obj.to_str().unwrap()],
    ));
    let exe = dir.join("out");
    ok(&Command::new("clang")
        .arg(&obj)
        .arg(dir.join("side.c"))
        .arg(&env.nabir)
        .args(["-pthread", "-lm", "-o"])
        .arg(&exe)
        .output()
        .unwrap());
    for _ in 0..5 {
        ok(&Command::new(&exe).output().unwrap());
    }
}
