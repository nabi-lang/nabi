//! Windows executables, objects and their debug info.

#[allow(clippy::wildcard_imports)]
use crate::native::*;

/// A stand-in Windows runtime (what a hello-sized program calls) and
/// the C side of the ABI checks, `long` widened to 64 bits.
pub(crate) const WIN_C_SIDE: &str = r#"
typedef __builtin_va_list va_list;
#define va_start(a, b) __builtin_va_start(a, b)
#define va_arg(a, t) __builtin_va_arg(a, t)
#define va_end(a) __builtin_va_end(a)
typedef __SIZE_TYPE__ usize;
#define long long long
typedef struct { const char *p; usize n; } NabiStr;
__declspec(thread) __attribute__((aligned(8))) unsigned char nabi_tls[16 * sizeof(void *) + 16];
int _write(int, const void *, unsigned);
void *calloc(usize, usize);
void exit(int);
void nabi_runtime_init(void) {}
void nabi_runtime_shutdown(void) {}
void nabi_runtime_yield(void) {}
void nabi_enter_native(void) {}
void nabi_leave_native(void) {}
int nabi_native_reenter(void) { return 0; }
void nabi_native_resume(int x) { (void)x; }
void nabi_nosp_enter(void) {}
void nabi_nosp_leave(void) {}
void nabi_hash_keys(unsigned long *out) { out[2] = 1; out[3] = 1; }
void nabi_gc_add_root(const void *w) { (void)w; }
void *nabi_gc_alloc_slow(usize size, usize align, unsigned t) {
    char *p = calloc(1, size + align + 16); *(unsigned *)p = t; return p;
}
void nabi_gc_register_type(unsigned id, const void *t) { (void)id; (void)t; }
void nabi_gc_register_types(unsigned b, const void *t, usize n) { (void)b; (void)t; (void)n; }
void nabi_gc_wb_slow(void *o, const void *s, usize n) { (void)o; (void)s; (void)n; }
void nabi_panic_at(NabiStr m, NabiStr at) { _write(2, m.p, (unsigned)m.n); (void)at; exit(101); }
int nabi_static_init_begin(unsigned char *s, const char *at, usize n) { (void)at; (void)n; if (*s == 0) { *s = 1; return 1; } return 0; }
void nabi_static_init_end(unsigned char *s) { *s = 2; }
usize nabi_str_len(NabiStr s) { return s.n; }
int nabi_str_cmp(NabiStr a, NabiStr b) {
    if (a.n != b.n) return 1;
    for (usize i = 0; i < a.n; i++) if (a.p[i] != b.p[i]) return 1;
    return 0;
}
int nabi_str_eq(NabiStr a, NabiStr b) { return !nabi_str_cmp(a, b); }
int nabi_str_eq_parked(NabiStr a, NabiStr b) { return nabi_str_eq(a, b); }
int nabi_str_cmp_parked(NabiStr a, NabiStr b) { return nabi_str_cmp(a, b); }
int nabi_stdio_write(int fd, NabiStr s, int nl) {
    _write(fd, s.p, (unsigned)s.n); if (nl) _write(fd, "\n", 1); return 0;
}
int side_count = 42;
void show(long v) {
    char b[32]; int n = 31; int neg = v < 0;
    unsigned long u = neg ? -(unsigned long)v : v;
    b[n] = '\n';
    do { b[--n] = '0' + u % 10; u /= 10; } while (u);
    if (neg) b[--n] = '-';
    _write(1, b + n, 32 - n);
}
typedef struct { float x, y, z; } V3;
typedef struct { long a, b, c; } Big;
typedef struct { signed char a; short b; } Small;
typedef struct { long a; double b; } Mix;
typedef struct { int a, b, c; } I3;
V3 v3_scale(V3 v, float k) { V3 r = { v.x * k, v.y * k, v.z * k }; return r; }
Big big_make(long a) { Big b = { a, a * 2, a * 3 }; return b; }
long big_sum(Big b) { return b.a + b.b + b.c; }
Small small_inc(Small s) { s.a += 1; s.b += 1; return s; }
Mix mix_twice(Mix m) { m.a *= 2; m.b *= 2; return m; }
long many(long a, long b, long c, long d, long e, I3 s, long f) {
    return a + b + c + d + e + s.a + s.b + s.c + f;
}
double fmany(double a, long b, double c, int d, float e, double f, long g) {
    return a + b * 10 + c * 100 + d * 1000 + e * 10000 + f * 100000 + g * 1000000;
}
double vmix(int n, ...) {
    va_list ap; va_start(ap, n); double t = 0;
    for (int i = 0; i < n; i++) { t += va_arg(ap, int); t += va_arg(ap, double); }
    va_end(ap); return t;
}
long cb_big(Big (*f)(Big)) { Big b = { 1, 2, 3 }; Big r = f(b); return r.a * 100 + r.b * 10 + r.c; }
Small cb_small(Small (*f)(Small)) { Small s = { 4, 5 }; return f(s); }
"#;

pub(crate) const WIN_NABI_SIDE: &str = r#"
struct V3 { x: f32; y: f32; z: f32; }
struct Big { a: i64; b: i64; c: i64; }
struct Small { a: i8; b: i16; }
struct Mix { a: i64; b: f64; }
struct I3 { a: i32; b: i32; c: i32; }

extern "C" {
    fn show(v: i64);
    fn v3_scale(v: V3, k: f32) -> V3;
    fn big_make(a: i64) -> Big;
    fn big_sum(b: Big) -> i64;
    fn small_inc(s: Small) -> Small;
    fn mix_twice(m: Mix) -> Mix;
    fn many(a: i64, b: i64, c: i64, d: i64, e: i64, s: I3, f: i64) -> i64;
    fn fmany(a: f64, b: i64, c: f64, d: i32, e: f32, f: f64, g: i64) -> f64;
    fn vmix(n: i32, ...) -> f64;
    fn cb_big(f: fn(Big) -> Big) -> i64;
    fn cb_small(f: fn(Small) -> Small) -> Small;
}

extern "C" fn rev_big(b: Big) -> Big { Big { a: b.c, b: b.b, c: b.a } }
extern "C" fn neg_small(s: Small) -> Small { Small { a: -s.a, b: -s.b } }

// nabi to nabi: arguments past the four register positions.
fn mixed(a: i64, s: str, b: f64, t: Big, c: i32, d: f32, u: str, e: i64) -> i64 {
    a + s.len() as i64 * 10 + (b * 100.0) as i64 + t.a * 1000 + t.c * 10000
        + c as i64 * 100000 + (d * 1000000.0) as i64 + u.len() as i64 * 10000000
        + e * 100000000
}

fn main() {
    println("hello from nabi");
    unsafe {
        let v = v3_scale(V3 { x: 1.0, y: 2.0, z: 3.0 }, 2.0);
        show((v.x * 100.0 + v.y * 10.0 + v.z) as i64);
        let b = big_make(5);
        show(b.a * 100 + b.b * 10 + b.c + big_sum(b) * 1000);
        let s = small_inc(Small { a: -3, b: 300 });
        show(s.a as i64 * 1000 + s.b as i64);
        let m = mix_twice(Mix { a: 21, b: 1.25 });
        show(m.a * 100 + (m.b * 10.0) as i64);
        show(many(1, 2, 3, 4, 5, I3 { a: 6, b: 7, c: 8 }, 9));
        show(fmany(1.0, 2, 3.0, 4, 5.0, 6.0, 7) as i64);
        let small: i8 = -2;
        let half: f32 = 0.5;
        show((vmix(2, small, half, true, 1.25) * 100.0) as i64);
        show(cb_big(rev_big));
        let n = cb_small(neg_small);
        show(n.a as i64 * 1000 + n.b as i64);
        show(mixed(1, "ab", 0.5, Big { a: 3, b: 0, c: 4 }, 5, 0.25, "xyz", 6));
        let k = "xyz";
        show(if k == "xyz" { 1 } else { 0 });
        // Data of a DLL, and of an object (lld-link makes its import
        // address).
        show(if NlsAnsiCodePage > 0u16 { 1 } else { 0 });
        side_count += 1;
        show(side_count as i64);
    }
}

#[link("ntdll")]
extern "system" {
    static NlsAnsiCodePage: u16;
}

extern "C" {
    static mut side_count: i32;
}
"#;

pub(crate) const WIN_EXPECTED: &str =
    "hello from nabi\n246\n30615\n-1699\n4225\n45\n7654321\n75\n321\n-4005\n630793071\n1\n1\n43\n";

/// `(machine, subsystem, unwind table present)` of a PE image.
pub(crate) fn pe_header(exe: &[u8]) -> (u16, u16, bool) {
    assert_eq!(&exe[..2], b"MZ");
    let pe = u32::from_le_bytes(exe[0x3c..0x40].try_into().unwrap()) as usize;
    assert_eq!(&exe[pe..pe + 4], b"PE\0\0");
    let opt = pe + 24;
    // The exception directory (`.pdata`): data directory 3, after a
    // PE32+ / PE32 optional header's 112 / 96 bytes.
    let dirs = if exe[opt] == 0x0b && exe[opt + 1] == 0x02 {
        112
    } else {
        96
    };
    let pdata = opt + dirs + 3 * 8 + 4;
    (
        u16::from_le_bytes([exe[pe + 4], exe[pe + 5]]),
        u16::from_le_bytes([exe[opt + 68], exe[opt + 69]]),
        u32::from_le_bytes(exe[pdata..pdata + 4].try_into().unwrap()) > 0,
    )
}

/// Windows executables from this host without an SDK; run under
/// `NABI_TEST_WINE` where given.
#[test]
fn windows_executables() {
    let env = setup();
    let dir = scratch("windows");
    fs::write(dir.join("side.c"), WIN_C_SIDE).unwrap();
    let pkg = dir.join("winpkg");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::write(pkg.join("src/main.nabi"), WIN_NABI_SIDE).unwrap();
    let wine = std::env::var_os("NABI_TEST_WINE").filter(|w| !w.is_empty());
    for (triple, machine, builds) in [
        ("x86_64-pc-windows-msvc", 0x8664, &["-O0", "-O2"][..]),
        ("aarch64-pc-windows-msvc", 0xaa64, &["-O0"][..]),
    ] {
        let side = dir.join(format!("{triple}.obj"));
        ok(&Command::new("clang")
            .args(["-target", triple, "-O1", "-c", "-o"])
            .arg(&side)
            .arg(dir.join("side.c"))
            .output()
            .unwrap());
        for (i, opt) in builds.iter().enumerate() {
            let profile = if *opt == "-O0" { "debug" } else { "release" };
            let exe = pkg.join(format!("target/{triple}/{profile}/winpkg.exe"));
            let gui = i == builds.len() - 1 && machine == 0xaa64;
            let mut nabic = Command::new(NABIC);
            nabic
                .args(["-q", "build"])
                .arg(&pkg)
                .arg("--bootstrap")
                .arg(&env.bootstrap)
                .arg("--nabir")
                .arg(&side)
                .args(["--target", triple, opt]);
            if gui {
                nabic.args(["-C", "subsystem=windows"]);
            }
            ok(&nabic.output().unwrap());
            let sub = if gui { 2 } else { 3 };
            assert_eq!(
                pe_header(&fs::read(&exe).unwrap()),
                (machine, sub, true),
                "{triple} {opt}"
            );
            if let (Some(wine), false) = (&wine, machine == 0xaa64) {
                let run = Command::new(wine)
                    .arg(&exe)
                    .env("WINEDEBUG", "-all")
                    .output()
                    .unwrap();
                assert_eq!(
                    text(&run.stdout).replace('\r', ""),
                    WIN_EXPECTED,
                    "{triple} {opt}"
                );
            }
        }
    }
}

/// The sections of COFF object `o`: `(name, data)` with `(offset,
/// symbol)` of each relocation; and its symbols, `(name, section)`
/// (section 0: undefined).
#[allow(clippy::type_complexity)]
pub(crate) fn coff(o: &[u8]) -> (Vec<(String, Vec<u8>, Vec<(u32, u32)>)>, Vec<(String, i16)>) {
    let u16at = |at: usize| u16::from_le_bytes([o[at], o[at + 1]]);
    let u32at = |at: usize| u32::from_le_bytes(o[at..at + 4].try_into().unwrap());
    let (nsect, symptr, nsyms) = (u16at(2) as usize, u32at(8) as usize, u32at(12) as usize);
    let strtab = symptr + 18 * nsyms;
    let name = |raw: &[u8]| -> String {
        if raw[..4] == [0; 4] {
            let at = strtab + u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
            let end = o[at..].iter().position(|&b| b == 0).unwrap();
            String::from_utf8_lossy(&o[at..at + end]).into_owned()
        } else {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(8);
            String::from_utf8_lossy(&raw[..end]).into_owned()
        }
    };
    let mut sects = Vec::new();
    for k in 0..nsect {
        let h = 20 + 40 * k;
        let (size, ptr) = (u32at(h + 16) as usize, u32at(h + 20) as usize);
        let (relptr, nrel) = (u32at(h + 24) as usize, u16at(h + 32) as usize);
        let relocs = (0..nrel)
            .map(|r| (u32at(relptr + 10 * r), u32at(relptr + 10 * r + 4)))
            .collect();
        let end = o[h..h + 8].iter().position(|&b| b == 0).unwrap_or(8);
        let n = String::from_utf8_lossy(&o[h..h + end]).into_owned();
        sects.push((n, o[ptr..ptr + size].to_vec(), relocs));
    }
    let mut syms = Vec::new();
    let mut i = 0;
    while i < nsyms {
        let at = symptr + 18 * i;
        syms.push((name(&o[at..at + 8]), u16at(at + 12) as i16));
        // Auxiliary records keep the symbol indices.
        for _ in 0..o[at + 17] {
            syms.push((String::new(), 0));
        }
        i += 1 + o[at + 17] as usize;
    }
    (sects, syms)
}

/// Windows `-O2 -g`: a float constant variable is an `S_LDATA32` of its
/// value in a `.rdata` section, which dbghelp reads (it reads no float
/// `S_CONSTANT`); the C entry nabi's startup calls is `nabi_main`, so
/// `main` is only the program's.
#[test]
fn codeview_float_constants_and_one_main() {
    let env = setup();
    let dir = scratch("cvfloat");
    let src = dir.join("main.nabi");
    fs::write(
        &src,
        "fn work(y: f64) -> f64 {\n    let k = 2.5;\n    let z = y * k;\n    \
         println(f\"{z}\");\n    z\n}\nfn main() {\n    work(3.0);\n}\n",
    )
    .unwrap();
    for triple in [
        "x86_64-pc-windows-msvc",
        "i686-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ] {
        let obj = dir.join(format!("{triple}.obj"));
        ok(&Command::new(NABIC)
            .args(["-q", "build"])
            .arg(&src)
            .arg("--bootstrap")
            .arg(&env.bootstrap)
            .args(["--target", triple, "-O2", "-g", "--emit=obj", "-o"])
            .arg(&obj)
            .output()
            .unwrap());
        let (sects, syms) = coff(&fs::read(&obj).unwrap());
        let defined = |n: &str| syms.iter().any(|(s, k)| s == n && *k > 0);
        let c = |n: &str| {
            if triple.starts_with("i686") {
                format!("_{n}")
            } else {
                n.to_owned()
            }
        };
        assert!(defined(&c("nabi_main")), "{triple}: no nabi_main");
        assert!(!defined(&c("main")), "{triple}: a C main");
        // `k`'s record, its value through the relocation of its offset.
        let mut found = false;
        for (_, data, relocs) in sects.iter().filter(|s| s.0 == ".debug$S") {
            let mut at = 4;
            while at + 8 <= data.len() {
                let kind = u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
                let len = u32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap()) as usize;
                let (start, end) = (at + 8, at + 8 + len);
                at = end.next_multiple_of(4);
                if kind != 0xf1 {
                    continue;
                }
                let mut r = start;
                while r + 4 <= end {
                    let rlen = u16::from_le_bytes([data[r], data[r + 1]]) as usize;
                    let rkind = u16::from_le_bytes([data[r + 2], data[r + 3]]);
                    let body = &data[r + 4..r + 2 + rlen];
                    if rkind == 0x110c && &body[10..] == b"k\0" {
                        assert_eq!(&body[..4], &0x41u32.to_le_bytes(), "{triple}: k's type");
                        let field = (r + 8) as u32;
                        let &(_, sym) = relocs
                            .iter()
                            .find(|x| x.0 == field)
                            .unwrap_or_else(|| panic!("{triple}: k unrelocated"));
                        let (rdata, value, _) = &sects[syms[sym as usize].1 as usize - 1];
                        assert_eq!(rdata, ".rdata", "{triple}");
                        let off = u32::from_le_bytes(body[4..8].try_into().unwrap()) as usize;
                        let v = f64::from_le_bytes(value[off..off + 8].try_into().unwrap());
                        assert_eq!(v.to_bits(), 2.5f64.to_bits(), "{triple}: k's value");
                        found = true;
                    }
                    r += 2 + rlen;
                }
            }
        }
        assert!(found, "{triple}: no S_LDATA32 `k`");
    }
}
