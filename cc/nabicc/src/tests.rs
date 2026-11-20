use crate::lex::{Kind, Lexer, Names};
use crate::parse::objc::{EnumKind, Null};
use crate::parse::{Parser, Val};
use crate::pp::Pp;
use crate::target::{Prim, Target, TargetInfo};
use crate::ty::Type;
use crate::{Config, parse};
use std::path::{Path, PathBuf};

const X64: TargetInfo<'static> = TargetInfo {
    triple: "x86_64-unknown-linux-gnu",
    arch: "x86_64",
    os: "linux",
    env: "gnu",
    vendor: "unknown",
    pointer_width: 64,
    big_endian: false,
};

const WIN32: TargetInfo<'static> = TargetInfo {
    triple: "i686-pc-windows-msvc",
    arch: "x86",
    os: "windows",
    env: "msvc",
    vendor: "pc",
    pointer_width: 32,
    big_endian: false,
};

const WIN64: TargetInfo<'static> = TargetInfo {
    triple: "x86_64-pc-windows-msvc",
    arch: "x86_64",
    os: "windows",
    env: "msvc",
    vendor: "pc",
    pointer_width: 64,
    big_endian: false,
};

const I686: TargetInfo<'static> = TargetInfo {
    triple: "i686-unknown-linux-gnu",
    arch: "x86",
    os: "linux",
    env: "gnu",
    vendor: "unknown",
    pointer_width: 32,
    big_endian: false,
};

const ARM: TargetInfo<'static> = TargetInfo {
    triple: "armv7-unknown-linux-gnueabihf",
    arch: "arm",
    os: "linux",
    env: "gnu",
    vendor: "unknown",
    pointer_width: 32,
    big_endian: false,
};

const A64: TargetInfo<'static> = TargetInfo {
    triple: "aarch64-unknown-linux-gnu",
    arch: "aarch64",
    os: "linux",
    env: "gnu",
    vendor: "unknown",
    pointer_width: 64,
    big_endian: false,
};

/// A scratch directory of its own for each test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nabicc_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn put(p: &Path, text: &str) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(p, text).unwrap();
}

/// Parse `src` as a header for `t`, then run `f`.
fn with<T>(
    name: &str,
    t: TargetInfo<'_>,
    src: &str,
    f: impl FnOnce(&mut Parser<'_, '_>) -> T,
) -> Result<T, String> {
    with_lang(name, t, src, false, f)
}

/// [`with`], as Objective-C when `objc`.
fn with_lang<T>(
    name: &str,
    t: TargetInfo<'_>,
    src: &str,
    objc: bool,
    f: impl FnOnce(&mut Parser<'_, '_>) -> T,
) -> Result<T, String> {
    let d = scratch(name);
    let h = d.join("h.h");
    put(&h, src);
    let cfg = Config {
        target: t,
        args: &[],
        host: false,
        sdk: None,
        objc,
    };
    let r = parse(&h, &cfg, f).map_err(|e| e.0);
    let _ = std::fs::remove_dir_all(&d);
    r
}

/// The preprocessed tokens of `src`, spaced.
fn pp_text(name: &str, src: &str) -> String {
    let d = scratch(name);
    let h = d.join("h.c");
    put(&h, src);
    let t = Target::new(&X64).unwrap();
    let dirs = crate::sysinc::dirs(&t, &crate::sysinc::Opts::default(), false, None);
    let mut pp = Pp::new(&t, dirs);
    pp.run_text(&t.predefines()).unwrap();
    pp.enter_main(&h, &[]).unwrap();
    let mut out = Vec::new();
    loop {
        let tok = pp.next().unwrap();
        if tok.kind == Kind::Eof {
            break;
        }
        out.push(pp.names.get(tok.sym).to_owned());
    }
    let _ = std::fs::remove_dir_all(&d);
    out.join(" ")
}

/// `text`'s tokens, spaced as `pp_text` spaces them.
fn spaced(text: &str) -> String {
    let mut names = Names::default();
    let mut lx = Lexer::new(1, text.as_bytes().into());
    let mut out = Vec::new();
    loop {
        let t = lx.next(&mut names);
        if t.kind == Kind::Eof {
            return out.join(" ");
        }
        out.push(names.get(t.sym).to_owned());
    }
}

#[test]
fn macro_examples_of_the_standard() {
    // C11 6.10.3.5, examples 3, 5 and 7.
    let src = r#"#define x 3
#define f(a) f(x * (a))
#undef x
#define x 2
#define g f
#define z z[0]
#define h g(~
#define m(a) a(w)
#define w 0,1
#define t(a) a
#define p() int
#define q(x) x
#define r(x,y) x ## y
#define str(x) # x
f(y+1) + f(f(z)) % t(t(g)(0) + t)(1);
g(x+(3,4)-w) | h 5) & m
(f)^m(m);
p() i[q()] = { q(1), r(2,3), r(4,), r(,5), r(,) };
char c[2][6] = { str(hello), str() };
#define tt(x,y,z) x ## y ## z
int j[] = { tt(1,2,3), tt(,4,5), tt(6,,7), tt(8,9,),
tt(10,,), tt(,11,), tt(,,12), tt(,,) };
#define debug(...) fprintf(stderr, __VA_ARGS__)
#define showlist(...) puts(#__VA_ARGS__)
#define report(test, ...) ((test)?puts(#test): printf(__VA_ARGS__))
debug("Flag");
debug("X = %d\n", x);
showlist(The first, second, and third items.);
report(x>y, "x is %d but y is %d", x, y);
"#;
    let want = r#"f(2 * (y+1)) + f(2 * (f(2 * (z[0])))) % f(2 * (0)) + t(1);
f(2 * (2+(3,4)-0,1)) | f(2 * (~ 5)) & f(2 * (0,1))^m(0,1);
int i[] = { 1, 23, 4, 5, };
char c[2][6] = { "hello", "" };
int j[] = { 123, 45, 67, 89, 10, 11, 12, };
fprintf(stderr, "Flag");
fprintf(stderr, "X = %d\n", 2);
puts("The first, second, and third items.");
((2>y)?puts("x>y"): printf("x is %d but y is %d", 2, y));
"#;
    assert_eq!(pp_text("examples", src), spaced(want));
}

#[test]
fn stringizing_and_pasting() {
    // C11 6.10.3.5 example 4, and GNU's `, ## __VA_ARGS__`.
    let src = r#"#define str(s) # s
#define xstr(s) str(s)
#define debug(s, t) printf("x" # s "= %d, x" # t "= %s", \
 x ## s, x ## t)
#define glue(a, b) a ## b
#define xglue(a, b) glue(a, b)
#define HIGHLOW "hello"
#define LOW LOW ", world"
debug(1, 2);
fputs(str(strncmp("abc\0d", "abc", '\4') // this goes away
 == 0) str(: @\n), s);
glue(HIGH, LOW);
xglue(HIGH, LOW)
#define e(fmt, ...) f(fmt, ## __VA_ARGS__)
e(a) e(a, b, c)
#define o(a, ...) g(a __VA_OPT__(,) __VA_ARGS__)
o(1) o(1, 2)
"#;
    let want = r#"printf("x" "1" "= %d, x" "2" "= %s", x1, x2);
fputs("strncmp(\"abc\\0d\", \"abc\", '\\4') == 0" ": @\n", s);
"hello";
"hello" ", world"
f(a) f(a, b, c)
g(1) g(1, 2)
"#;
    assert_eq!(pp_text("stringize", src), spaced(want));
}

#[test]
fn conditional_arithmetic() {
    let src = r"#define TWO 2
#if -1 > 0u
unsigned_wins
#endif
#if 1 || (1 / 0)
short_circuit
#endif
#if defined TWO && TWO * 3 == 6 && !defined(THREE)
defined_ok
#elif 1
wrong
#endif
#if 'A' == 65 && (0x10 >> 2) == 4 && (3 ? 4 : 5) == 4
chars_ok
#else
wrong
#endif
#ifdef __x86_64__
target_ok
#endif
#if __has_include(<stddef.h>) && !__has_include(<nope.h>)
has_include_ok
#endif
#if __has_attribute(packed) && !__has_attribute(no_such_thing)
attribute_ok
#endif
";
    assert_eq!(
        pp_text("cond", src),
        "unsigned_wins short_circuit defined_ok chars_ok target_ok has_include_ok attribute_ok"
    );
}

#[test]
fn includes_once_guards_and_next() {
    let d = scratch("includes");
    put(
        &d.join("a/x.h"),
        "#pragma once\nfrom_a\n#include_next <x.h>\n",
    );
    put(&d.join("b/x.h"), "#ifndef XB\n#define XB\nfrom_b\n#endif\n");
    put(
        &d.join("main.h"),
        "#include <x.h>\n#include <x.h>\n#include \"b/x.h\"\n",
    );
    let t = Target::new(&X64).unwrap();
    let dir = |p: &str| crate::pp::Dir {
        path: d.join(p),
        system: false,
        kind: crate::pp::DirKind::Normal,
    };
    let mut pp = Pp::new(&t, vec![dir("a"), dir("b")]);
    pp.enter_main(&d.join("main.h"), &[]).unwrap();
    let mut out = Vec::new();
    loop {
        let tok = pp.next().unwrap();
        if tok.kind == Kind::Eof {
            break;
        }
        out.push(pp.names.get(tok.sym).to_owned());
    }
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(out, ["from_a", "from_b"]);
}

/// Size, alignment and field offsets (bits) of `struct s`.
fn layout(t: TargetInfo<'_>, body: &str) -> (u64, u64, Vec<u64>) {
    with("layout", t, body, |p| {
        let r = p
            .types
            .records
            .iter()
            .position(|r| r.name.as_deref() == Some("s"))
            .expect("struct s") as u32;
        let l = p.types.layout(r).expect("a layout");
        (l.size, l.align, l.offsets)
    })
    .unwrap()
}

#[test]
fn record_layouts() {
    let flags = "struct s { unsigned a : 3; unsigned b : 5; int c; unsigned d : 1; };";
    assert_eq!(layout(X64, flags), (12, 4, vec![0, 3, 32, 64]));
    // Microsoft's bitfields: a new unit when the type's size changes.
    let mixed = "struct s { char a : 3; short b : 9; int c : 20; };";
    assert_eq!(layout(X64, mixed), (4, 4, vec![0, 3, 12]));
    assert_eq!(layout(WIN64, mixed), (8, 4, vec![0, 16, 32]));
    // i386 System V aligns `long long` and `double` to 4.
    let wide = "struct s { char c; long long l; double d; };";
    assert_eq!(layout(I686, wide), (20, 4, vec![0, 32, 96]));
    assert_eq!(layout(WIN32, wide), (24, 8, vec![0, 64, 128]));
    // `#pragma pack` and `packed`.
    let packed = "#pragma pack(push, 2)\nstruct s { char c; int i; double d; };\n#pragma pack(pop)";
    assert_eq!(layout(X64, packed), (14, 2, vec![0, 16, 48]));
    let attr = "struct s { char c; int i; } __attribute__((packed));";
    assert_eq!(layout(X64, attr), (5, 1, vec![0, 8]));
    // Unnamed bitfields set the alignment on ARM only.
    let unnamed = "struct s { char c; int : 3; };";
    assert_eq!(layout(X64, unnamed).0, 2);
    assert_eq!(layout(ARM, unnamed).0, 4);
    // Flexible arrays, `aligned` and anonymous members.
    let flex = "struct s { short n; struct { char a; long long b; }; double tail[]; };";
    assert_eq!(layout(X64, flex), (24, 8, vec![0, 64, 192]));
    let aligned = "typedef int a2 __attribute__((aligned(2)));\nstruct s { char c; a2 i; };";
    assert_eq!(layout(X64, aligned), (6, 2, vec![0, 16]));
    assert_eq!(layout(WIN64, aligned), (8, 4, vec![0, 32]));
    // Microsoft's 32-bit pointers, and a union's flexible array as one
    // element.
    let ptr32 = "struct s { char c; char * __ptr32 p; };";
    assert_eq!(layout(WIN64, ptr32), (8, 4, vec![0, 32]));
    let flex_union = "struct e { int a, b, c; };\nstruct s { int n; union { struct e x[]; }; };";
    assert_eq!(layout(WIN64, flex_union), (16, 4, vec![0, 32]));
    assert_eq!(layout(X64, flex_union), (4, 4, vec![0, 32]));
}

/// The integer type of `enum e`.
fn enum_int(t: TargetInfo<'_>, body: &str) -> Prim {
    with("enum", t, body, |p| {
        let e = p
            .types
            .enums
            .iter()
            .find(|e| e.name.as_deref() == Some("e"))
            .expect("enum e");
        p.types.prim_of(e.int.expect("complete")).unwrap()
    })
    .unwrap()
}

#[test]
fn enum_types() {
    assert_eq!(enum_int(X64, "enum e { A, B = 5 };"), Prim::UInt);
    assert_eq!(enum_int(X64, "enum e { A = -1, B };"), Prim::Int);
    assert_eq!(enum_int(X64, "enum e { A = 0x100000000 };"), Prim::ULong);
    assert_eq!(enum_int(WIN64, "enum e { A, B = 5 };"), Prim::Int);
    assert_eq!(
        enum_int(X64, "enum __attribute__((packed)) e { A = 200 };"),
        Prim::UChar
    );
    assert_eq!(
        enum_int(X64, "enum e : unsigned short { A };"),
        Prim::UShort
    );
    // MSVC's `int` enums hold their values as `int`.
    let v = with("enum_ms", WIN64, "enum e { A = 0x80000000, B };", |p| {
        p.types.enums[0]
            .values
            .iter()
            .map(|v| v.value)
            .collect::<Vec<_>>()
    })
    .unwrap();
    assert_eq!(v, [-0x8000_0000, -0x7fff_ffff]);
}

/// `expr`'s type and value after `decls`.
fn eval(decls: &str, expr: &str) -> (String, Val) {
    let src = format!("{decls}\n#define EXPR {expr}\n");
    with("eval", X64, &src, |p| {
        let s = p.pp.names.lookup("EXPR").unwrap();
        let toks = p.pp.expand_name(s).unwrap();
        let e = p.eval_tokens(toks).expect("a constant");
        let ty = match p.types.get(p.types.canon(e.ty.ty)) {
            Type::Prim(x) => x.name().to_owned(),
            _ => p.types.spell(e.ty),
        };
        (ty, e.v)
    })
    .unwrap()
}

#[test]
fn constant_expressions() {
    let int = |d: &str, e: &str| match eval(d, e) {
        (t, Val::Int(v)) => (t, v),
        other => panic!("{e}: {other:?}"),
    };
    assert_eq!(int("", "1"), ("int".to_owned(), 1));
    assert_eq!(int("", "1u << 4"), ("unsigned int".to_owned(), 16));
    assert_eq!(
        int("", "0xffffffff"),
        ("unsigned int".to_owned(), 0xffff_ffff)
    );
    assert_eq!(int("", "4294967296"), ("long".to_owned(), 1 << 32));
    assert_eq!(int("", "-1 + 0u"), ("unsigned int".to_owned(), 0xffff_ffff));
    assert_eq!(int("", "'a'"), ("int".to_owned(), 97));
    assert_eq!(int("", "(char)300"), ("char".to_owned(), 44));
    assert_eq!(
        int("struct t { char c; double d; };", "sizeof(struct t)"),
        ("unsigned long".to_owned(), 16)
    );
    assert_eq!(
        int(
            "struct t { char c; int x[4]; };",
            "__builtin_offsetof(struct t, x[2])"
        ),
        ("unsigned long".to_owned(), 12)
    );
    assert_eq!(
        int(
            "struct t { char c; int x; };",
            "((unsigned long)&((struct t *)0)->x)"
        ),
        ("unsigned long".to_owned(), 4)
    );
    assert_eq!(int("enum { A = 3, B };", "B * 2"), ("int".to_owned(), 8));
    assert!(
        matches!(eval("", "1.5f"), (t, Val::Float(x)) if t == "float" && x.to_bits() == 1.5f64.to_bits())
    );
    assert!(matches!(eval("", "0x1p-2"), (_, Val::Float(x)) if x.to_bits() == 0.25f64.to_bits()));
    assert!(matches!(eval("", "\"a\" \"b\""), (_, Val::Str(s)) if s == b"ab"));
    assert!(matches!(eval("extern int v;", "v + 1"), (_, Val::None)));
}

#[test]
fn errors_point_at_the_header() {
    let err = |src: &str| with("err", X64, src, |_| ()).unwrap_err();
    let e = err("int f(;\n");
    assert!(
        e.contains("h.h:1:7: error: expected a C declaration, found ';'"),
        "{e}"
    );
    let e = err("\nfoo_t x;\n");
    assert!(
        e.contains("h.h:2:1: error: unknown type name 'foo_t'"),
        "{e}"
    );
    let e = err("#include <missing.h>\n");
    assert!(
        e.contains("h.h:1:10: error: 'missing.h' file not found"),
        "{e}"
    );
    let e = err("#if 1\nint x;\n");
    assert!(
        e.contains("h.h:1:2: error: unterminated conditional directive"),
        "{e}"
    );
    let e = err("int f(a, b) int a; int b; { return 0; }\n");
    assert!(e.contains("K&R-style function definitions"), "{e}");
    let e = err("_Static_assert(sizeof(int) == 8, \"int\");\n");
    assert!(e.contains("static assertion failed: int"), "{e}");
    let e = err("int x;\nextern \"C\" {\n");
    assert!(
        e.contains("h.h:2:8: error: expected a C declaration, found '\"C\"'"),
        "{e}"
    );
    let e = err("@interface A\n@end\n");
    assert!(
        e.contains("h.h:1:1: error: expected a C declaration, found '@'"),
        "{e}"
    );
}

#[test]
fn intrinsic_headers() {
    let x86 = "#include <x86intrin.h>\n#include <nmmintrin.h>\n#include <wmmintrin.h>\n\
               _Static_assert(sizeof(__m512) == 64 && _Alignof(__m256d) == 32, \"\");\n\
               _Static_assert(sizeof(__m128i) == 16 && sizeof(__m64) == 8, \"\");\n\
               struct s { char c; __m256 v; };\n\
               _Static_assert(sizeof(struct s) == 64, \"\");\n";
    for t in [X64, I686, WIN64, WIN32] {
        let n = with("x86", t, x86, |p| {
            p.fns.iter().filter(|f| f.name.starts_with("_mm")).count()
        });
        assert!(n.unwrap() > 500);
    }
    let arm = "#include <arm_neon.h>\n#include <arm_acle.h>\n#include <arm_fp16.h>\n\
               _Static_assert(sizeof(float32x4_t) == 16 && sizeof(int8x16x2_t) == 32, \"\");\n\
               _Static_assert(_Alignof(int64x2_t) == ALIGN, \"\");\n";
    for (t, align) in [(A64, 16), (ARM, 8)] {
        let src = format!("#define ALIGN {align}\n{arm}");
        let n = with("arm", t, &src, |p| {
            p.fns.iter().filter(|f| f.name.starts_with("vadd")).count()
        });
        assert!(n.unwrap() > 20);
    }
    // A header for another architecture says so, as clang's do.
    let e = with("x86arm", A64, "#include <immintrin.h>\n", |_| ()).unwrap_err();
    assert!(e.contains("only meant to be used on x86"), "{e}");
    let e = with("armx86", X64, "#include <arm_neon.h>\n", |_| ()).unwrap_err();
    assert!(e.contains("intended only for ARM and AArch64"), "{e}");
}

/// C that every target compiles: structs by value, unions, bitfields,
/// switch, goto, function pointers, data with addresses, `va_arg`.
const PROGRAM: &str = r#"
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
struct P { int x, y; };
struct D { double a, b; };
union U { int i; float f; };
struct B { unsigned a : 3; signed b : 5; unsigned c : 12; };
static int add(int a, int b) { return a + b; }
static int (*const ops[])(int, int) = { add };
static const char *names[] = { "zero", "one" };
struct P mkp(int x, int y) { struct P p = { .y = y, .x = x }; return p; }
struct D twice(struct D d) { d.a *= 2; d.b *= 2; return d; }
int va_sum(int n, va_list ap) {
    int s = 0;
    while (n-- > 0) s += va_arg(ap, int);
    return s;
}
int f(int x, struct B *b, union U *u) {
    int r = 0;
    switch (x) {
    case 0: r = 1;
    case 1: r += 2; break;
    case 2 ... 9: r = ops[0](x, 3); break;
    default: goto out;
    }
    b->b -= 1;
    u->f = 1.5f;
out:
    return r + names[x & 1][0] + (int)offsetof(struct P, y) + (int)sizeof(int64_t);
}
"#;

#[test]
fn compiles_c_for_every_target() {
    let d = scratch("every");
    let src = d.join("p.c");
    put(&src, PROGRAM);
    let flags = crate::flags::Flags {
        mode: crate::flags::Mode::Compile,
        pp: vec!["-nostdlibinc".to_owned()],
        opt: "2".to_owned(),
        ..crate::flags::Flags::default()
    };
    for name in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
        "i686-unknown-linux-gnu",
        "i686-pc-windows-msvc",
        "armv7-unknown-linux-gnueabihf",
        "riscv64gc-unknown-linux-gnu",
        "powerpc64le-unknown-linux-gnu",
        "s390x-unknown-linux-gnu",
        "riscv32imac-unknown-none-elf",
        "wasm32-wasip2",
        "wasm32-wasip1-threads",
        "wasm32-unknown-unknown",
    ] {
        let t = nabic_target::TargetSpec::resolve_any(Some(name)).unwrap();
        for opt in ["0", "2"] {
            let f = crate::flags::Flags {
                opt: opt.to_owned(),
                pp: flags.pp.clone(),
                mode: flags.mode,
                ..crate::flags::Flags::default()
            };
            let o = crate::unit::compile(&src, &f, &t)
                .unwrap_or_else(|e| panic!("{name} -O{opt}: {e}"));
            assert!(!o.bytes.is_empty(), "{name}");
        }
    }
}

/// Every refusal says what to write instead.
#[test]
fn refusals_say_what_to_write() {
    let d = scratch("refuse");
    let t = nabic_target::TargetSpec::resolve(Some("x86_64-unknown-linux-gnu")).unwrap();
    let f = crate::flags::Flags {
        mode: crate::flags::Mode::Compile,
        pp: vec!["-nostdlibinc".to_owned()],
        ..crate::flags::Flags::default()
    };
    for (i, (src, want)) in [
        ("void f(int n) { int a[n]; a[0] = 1; }", "malloc"),
        (
            "void f(void) { __asm__(\"cpuid\"); }",
            "write the code in C",
        ),
        ("void f(void) { void *p = &&l; l: ; }", "switch"),
        ("int f(void) { return g(); }", "prototype"),
        ("_Thread_local int x;", "thread"),
        ("void f(void) { __builtin_alloca(4); }", "malloc"),
    ]
    .into_iter()
    .enumerate()
    {
        let p = d.join(format!("r{i}.c"));
        put(&p, src);
        let e = crate::unit::compile(&p, &f, &t).err().unwrap_or_default();
        assert!(e.contains(want), "{src}: {e}");
    }
}

const MAC: TargetInfo<'static> = TargetInfo {
    triple: "arm64-apple-macosx11.0",
    arch: "aarch64",
    os: "macos",
    env: "",
    vendor: "apple",
    pointer_width: 64,
    big_endian: false,
};

/// `__is_target_os` / `__is_target_environment` and the platform macros
/// as the iOS SDK's availability headers read them.
#[test]
fn ios_target_queries() {
    let sim = TargetInfo {
        triple: "arm64-apple-ios15.0-simulator",
        os: "ios",
        ..MAC
    };
    let src = "_Static_assert(__is_target_os(ios) && !__is_target_os(macos), \"os\");\n\
               _Static_assert(__is_target_environment(simulator), \"env\");\n\
               _Static_assert(TARGET_OS_IOS && !TARGET_OS_OSX && TARGET_OS_SIMULATOR, \"macros\");\n";
    with_lang("ios", sim, src, false, |_| ()).unwrap();
    let mac = "_Static_assert(__is_target_os(macos) && !__is_target_os(ios), \"os\");\n\
               _Static_assert(!__is_target_environment(simulator) && TARGET_OS_OSX, \"mac\");\n";
    with_lang("mac", MAC, mac, false, |_| ()).unwrap();
}

#[test]
fn objective_c_declarations() {
    let src = r#"
typedef struct objc_object *id;
typedef struct objc_class *Class;
typedef signed char BOOL;
#if __OBJC__ && __has_feature(objc_arc) && __has_feature(objc_instancetype)
#define OK 1
#endif
_Static_assert(OK, "objc predefines");
@class NSString, NSError, NSObject, NSArray<ObjectType>;
@protocol NSCopying;
@protocol NSObject
- (BOOL)isEqual:(id)object;
@end
typedef long NSInteger;
typedef enum __attribute__((flag_enum,enum_extensibility(open))) Opts : NSInteger Opts;
enum Opts : NSInteger { OptA = 1, OptB = 2 };
static const NSInteger Five = 5;
enum { Six = Five + 1 };
#pragma clang assume_nonnull begin
__attribute__((availability(macos,introduced=10.12)))
@interface Box<__covariant ObjectType : id<NSCopying>> : NSObject <NSCopying, NSObject> {
    int hidden;
}
@property (nonatomic, readonly, getter=isOpen) BOOL open;
@property (nullable, copy) NSString *title;
@property (class, readonly) Box *shared;
+ (instancetype)boxWithObject:(ObjectType)o;
- (nullable ObjectType)objectAtIndex:(NSInteger)i error:(NSError * _Nullable * _Nullable)error __attribute__((availability(macos,introduced=13.3)));
- (void)each:(void (^)(ObjectType _Nonnull o, BOOL *stop))block;
- (NSArray<NSArray<id<NSCopying>> *> *)nested;
- (id)init __attribute__((unavailable));
@end
@interface Box (Extra)
- (void)extra;
@end
#pragma clang assume_nonnull end
@compatibility_alias Crate Box;
Crate *make(void);
"#;
    let ok = with_lang("objc", MAC, src, true, |p| {
        let o = &p.objc;
        assert_eq!(o.interfaces.len(), 2);
        let b = &o.interfaces[0];
        assert_eq!(
            (b.name.as_str(), b.sup.as_deref(), b.params.clone()),
            ("Box", Some("NSObject"), vec!["ObjectType".to_owned()])
        );
        assert_eq!(b.protocols, ["NSCopying", "NSObject"]);
        assert_eq!(b.avail.introduced, Some((10, 12, 0)));
        let sels: Vec<&str> = b.methods.iter().map(|m| m.sel.as_str()).collect();
        assert_eq!(
            sels,
            [
                "boxWithObject:",
                "objectAtIndex:error:",
                "each:",
                "nested",
                "init"
            ]
        );
        assert!(!b.methods[0].instance && b.methods[1].instance);
        assert!(b.methods.iter().all(|m| m.assume_nonnull));
        assert_eq!(b.methods[1].avail.introduced, Some((13, 3, 0)));
        assert_eq!(o.null.get(&b.methods[1].ret.ty), Some(&Null::Nullable));
        assert!(b.methods[4].avail.unavailable);
        let props: Vec<(&str, bool, Option<&str>)> = b
            .props
            .iter()
            .map(|x| (x.name.as_str(), x.class, x.getter.as_deref()))
            .collect();
        assert_eq!(
            props,
            [
                ("open", false, Some("isOpen")),
                ("title", false, None),
                ("shared", true, None)
            ]
        );
        assert_eq!(b.props[1].null, Some(Null::Nullable));
        assert_eq!(o.interfaces[1].category.as_deref(), Some("Extra"));
        assert_eq!(o.protocols.len(), 1);
        assert_eq!(
            o.enums.values().copied().collect::<Vec<_>>(),
            [EnumKind::Options]
        );
        let six = p
            .types
            .enums
            .iter()
            .flat_map(|e| &e.values)
            .find(|v| v.name == "Six");
        assert_eq!(six.map(|v| v.value), Some(6));
        p.fns.iter().any(|f| f.name == "make")
    })
    .unwrap();
    assert!(ok);
    // Headers declare; bodies are refused with what to do.
    let e = with_lang("objc_impl", MAC, "@implementation X\n@end\n", true, |_| ()).unwrap_err();
    assert!(e.contains("move the implementation to a `.m` file"), "{e}");
}

/// Android: Linux with bionic's macros, the API level from the triple,
/// and an IEEE quad `long double` on `x86_64` as on aarch64.
#[test]
fn android_predefines() {
    let info = TargetInfo {
        triple: "x86_64-linux-android31",
        os: "android",
        env: "",
        ..X64
    };
    let t = Target::new(&info).unwrap();
    assert_eq!(t.android, 31);
    let p = t.predefines();
    for d in [
        "#define __ANDROID__ 1",
        "#define __ANDROID_API__ 31",
        "#define __linux__ 1",
        "#define __SIZEOF_LONG_DOUBLE__ 16",
    ] {
        assert!(p.contains(d), "{d}");
    }
    assert!(!p.contains("__gnu_linux__"));
    assert!(
        !Target::new(&X64)
            .unwrap()
            .predefines()
            .contains("__ANDROID__")
    );
}

/// gcc's and clang's armv7 triples name the armv7 target of their C
/// library: a musl one is not taken for glibc.
#[test]
fn armv7_triples_keep_their_libc() {
    let name = |n: &str| match crate::resolve_target(Some(n)) {
        Ok(t) => t.name,
        Err(e) => e,
    };
    for (n, want) in [
        ("armv7-linux-gnueabihf", "armv7-unknown-linux-gnueabihf"),
        ("arm-linux-gnueabi", "armv7-unknown-linux-gnueabihf"),
        ("armv7-linux-musleabihf", "armv7-unknown-linux-musleabihf"),
        (
            "armv7l-unknown-linux-musl",
            "armv7-unknown-linux-musleabihf",
        ),
        ("arm-linux-musleabihf", "armv7-unknown-linux-musleabihf"),
    ] {
        let got = name(n);
        assert!(
            got.contains(want)
                && !got.contains(if want.contains("musl") {
                    "gnueabihf"
                } else {
                    "musl"
                }),
            "{n}: {got}"
        );
    }
}
