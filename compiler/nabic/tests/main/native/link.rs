//! Linking: attributes, library kinds, flags and dropped code.

#[allow(clippy::wildcard_imports)]
use crate::native::*;

/// `-O0` puts each function in its own section, so unused prebuilt code
/// is dropped at link time.
#[test]
fn unused_dependency_code_is_dropped() {
    let _ = setup();
    let targets = Path::new(NABIC)
        .parent()
        .unwrap()
        .join("../lib/nabi/targets");
    let dir = scratch("gc-sections");
    fs::create_dir_all(dir.join("dep/src")).unwrap();
    fs::write(
        dir.join("dep/src/lib.nabi"),
        "pub fn used() -> str {\n    return \"marker-of-used-code\";\n}\n\n\
         pub fn unused() -> str {\n    return \"marker-of-unused-code\";\n}\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app/src")).unwrap();
    fs::write(
        dir.join("app/src/main.nabi"),
        "use dep::used;\nprintln(used());\n",
    )
    .unwrap();
    let has = |b: &[u8], s: &str| b.windows(s.len()).any(|w| w == s.as_bytes());
    for t in [
        None,
        Some("x86_64-unknown-linux-gnu"),
        Some("aarch64-unknown-linux-gnu"),
        Some("x86_64-unknown-linux-musl"),
        Some("aarch64-unknown-linux-musl"),
        Some("x86_64-pc-windows-msvc"),
        Some("aarch64-pc-windows-msvc"),
    ] {
        if t.is_some_and(|t| !targets.join(t).is_dir()) {
            continue;
        }
        let exe = dir.join(format!("app-{}", t.unwrap_or("host")));
        let mut nabic = Command::new(NABIC);
        nabic
            .current_dir(&dir)
            .env("NABI_HOME", crate::common::workspace())
            .args(["-q", "build", "app", "--extern", "dep=dep", "-O0", "-o"])
            .arg(&exe);
        if let Some(t) = t {
            nabic.args(["--target", t]);
        }
        ok(&nabic.output().unwrap());
        let b = fs::read(&exe).unwrap();
        assert!(has(&b, "marker-of-used-code"), "{t:?}");
        assert!(!has(&b, "marker-of-unused-code"), "{t:?}");
    }
}

/// `#[link(..)]` on an `extern` block puts the library on the link
/// line; `LIBRARY_PATH` is the C driver's search path.
#[test]
fn link_attribute_links_the_library() {
    let env = setup();
    let dir = scratch("link_attr");
    fs::write(
        dir.join("probe.c"),
        "int nabi_link_probe(int x) { return x * 3; }\n",
    )
    .unwrap();
    let obj = dir.join("probe.o");
    ok(&Command::new("clang")
        .args(["-c", "-o"])
        .arg(&obj)
        .arg(dir.join("probe.c"))
        .output()
        .unwrap());
    for lib in ["nabilinka", "nabilinkb"] {
        ok(&Command::new("ar")
            .arg("rcs")
            .arg(dir.join(format!("lib{lib}.a")))
            .arg(&obj)
            .output()
            .unwrap());
    }
    let src = r#"
#[link("nabilinka")]
extern "C" {
    fn nabi_link_probe(x: i32) -> i32;
}

#[link(name = "nabilinkb", kind = "static")]
extern "C" {}

fn main() -> i32 {
    unsafe { nabi_link_probe(14) }
}
"#;
    fs::write(dir.join("main.nabi"), src).unwrap();
    let exe = dir.join("out");
    let out = Command::new(NABIC)
        .arg("-q")
        .arg("build")
        .arg(dir.join("main.nabi"))
        .arg("--bootstrap")
        .arg(&env.bootstrap)
        .arg("--nabir")
        .arg(&env.nabir)
        .arg("-o")
        .arg(&exe)
        .env("LIBRARY_PATH", &dir)
        .output()
        .unwrap();
    ok(&out);
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(run.status.code(), Some(42));
}

/// `kind = "static"` takes `lib<name>.a` from the `-L` directories even
/// beside a shared library of the same name (ld64 would take the
/// `.dylib`); a missing archive is an error naming it.
#[test]
fn static_link_kind_takes_the_archive() {
    let env = setup();
    let dir = scratch("link_static");
    let libs = dir.join("libs");
    fs::create_dir_all(&libs).unwrap();
    let c = |file: &str, body: &str, args: &[&str]| {
        let src = dir.join(format!("{file}.c"));
        fs::write(&src, body).unwrap();
        ok(&Command::new("clang")
            .args(args)
            .arg("-o")
            .arg(dir.join(file))
            .arg(&src)
            .output()
            .unwrap());
    };
    c(
        "a.o",
        "int nabi_static_probe(int x) { return x * 3; }\n",
        &["-c"],
    );
    ok(&Command::new("ar")
        .arg("rcs")
        .arg(libs.join("libnabistatic.a"))
        .arg(dir.join("a.o"))
        .output()
        .unwrap());
    let shared = if cfg!(target_os = "macos") {
        "libnabistatic.dylib"
    } else {
        "libnabistatic.so"
    };
    c(
        &format!("libs/{shared}"),
        "int nabi_static_probe(int x) { return x * 5; }\n",
        &["-shared", "-fPIC"],
    );
    let src = r#"
#[link(name = "nabistatic", kind = "static")]
extern "C" {
    fn nabi_static_probe(x: i32) -> i32;
}

#[link(name = "nabimissing", kind = "static")]
#[cfg(missing)]
extern "C" {}

fn main() -> i32 {
    unsafe { nabi_static_probe(14) }
}
"#;
    fs::write(dir.join("main.nabi"), src).unwrap();
    let exe = dir.join("out");
    let exe_arg = exe.display().to_string();
    let lib_arg = format!("-L{}", libs.display());
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &[&lib_arg, "-o", &exe_arg],
    ));
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(run.status.code(), Some(42));
    let out = build(
        &env,
        &dir.join("main.nabi"),
        &[
            "-L",
            &libs.display().to_string(),
            "--cfg",
            "missing",
            "-o",
            &exe_arg,
        ],
    );
    assert!(!out.status.success());
    // ld64 has no -Bstatic: nabic looks for the archive itself there.
    let want = if cfg!(target_os = "macos") {
        "no libnabimissing.a in the -L directories"
    } else {
        "unable to find library -lnabimissing"
    };
    assert!(text(&out.stderr).contains(want), "{}", text(&out.stderr));
}

/// `-l` links a library the source does not name; `--native-runtime`
/// copies a shared library beside the executable, which then loads it
/// from its own directory (rpath `@loader_path` / `$ORIGIN`).
#[cfg(unix)]
#[test]
#[cfg_attr(
    target_env = "musl",
    ignore = "a static musl toolchain links no shared library (`-l dylib=`)"
)]
fn link_flags_and_runtime_files() {
    let env = setup();
    let dir = scratch("link_runtime");
    let libs = dir.join("libs");
    fs::create_dir_all(&libs).unwrap();
    let (shared, id) = if cfg!(target_os = "macos") {
        (
            "libnabirt.dylib",
            "-Wl,-install_name,@rpath/libnabirt.dylib",
        )
    } else {
        ("libnabirt.so", "-Wl,-soname,libnabirt.so")
    };
    fs::write(
        dir.join("rt.c"),
        "int nabi_rt_probe(int x) { return x * 3; }\n",
    )
    .unwrap();
    ok(&Command::new("clang")
        .args(["-shared", "-fPIC", id, "-o"])
        .arg(libs.join(shared))
        .arg(dir.join("rt.c"))
        .output()
        .unwrap());
    let src = "extern \"C\" {\n    fn nabi_rt_probe(x: i32) -> i32;\n}\n\nfn main() -> i32 {\n    \
               unsafe { nabi_rt_probe(14) }\n}\n";
    fs::write(dir.join("main.nabi"), src).unwrap();
    let out_dir = dir.join("out");
    fs::create_dir_all(&out_dir).unwrap();
    let exe = out_dir.join("app");
    let lib_dir = format!("-L{}", libs.display());
    let runtime = format!("--native-runtime={}", libs.join(shared).display());
    let exe_arg = exe.display().to_string();
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &[&lib_dir, "-l", "dylib=nabirt", &runtime, "-o", &exe_arg],
    ));
    assert!(out_dir.join(shared).is_file());
    // The library's own directory is gone: only the copy can be loaded.
    fs::remove_dir_all(&libs).unwrap();
    let run = Command::new(&exe).current_dir(&dir).output().unwrap();
    assert_eq!(run.status.code(), Some(42), "{}", text(&run.stderr));
    let out = build(&env, &dir.join("main.nabi"), &["-l", "shared=x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("expected [static=|dylib=|framework=]<name>"),
        "{}",
        text(&out.stderr)
    );
}

/// `--crate-type=cdylib`: a shared library C opens with `dlopen` and
/// calls on its own thread and on threads it starts; the runtime comes
/// up when the library loads.
#[test]
fn cdylib_loads_from_c() {
    let _ = setup();
    let host = nabic_target::TargetSpec::host();
    if !nabild::links_shared(host) {
        return;
    }
    let dir = scratch("cdylib");
    fs::create_dir_all(dir.join("geo/src")).unwrap();
    fs::write(
        dir.join("geo/src/lib.nabi"),
        r#"#[export_name("geo_add")]
pub extern "C" fn add(a: i64, b: i64) -> i64 { a + b }

#[export_name("geo_sum_squares")]
pub extern "C" fn sum_squares(n: i64) -> i64 {
    let xs: List<i64> = [];
    for i in 0..n { xs.push(i * i); }
    let s = 0;
    for x in xs { s += x; }
    s
}

#[export_name("geo_text_len")]
pub extern "C" fn text_len(n: i64) -> i64 {
    let t = "";
    for i in 0..n { t = f"{t}{i},"; }
    t.len() as i64
}

pub fn hidden() -> i64 { 7 }
"#,
    )
    .unwrap();
    fs::write(
        dir.join("host.c"),
        r#"#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>
typedef long long (*f1)(long long);
static f1 sq;
static void *worker(void *arg) {
    long long s = 0;
    for (int i = 0; i < 50; i++) s += sq(1000);
    *(long long *)arg = s;
    return 0;
}
int main(int argc, char **argv) {
    void *h = dlopen(argv[1], RTLD_NOW);
    if (!h) { printf("%s\n", dlerror()); return 1; }
    long long (*add)(long long, long long) = dlsym(h, "geo_add");
    sq = dlsym(h, "geo_sum_squares");
    f1 tl = dlsym(h, "geo_text_len");
    printf("%lld %lld %lld %d\n", add(2, 40), sq(10), tl(1000), dlsym(h, "main") == 0);
    pthread_t t[4];
    long long r[4];
    for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, worker, &r[i]);
    for (int i = 0; i < 4; i++) pthread_join(t[i], 0);
    printf("%lld %lld\n", r[0], r[3]);
    return 0;
}
"#,
    )
    .unwrap();
    let so = dir.join(if cfg!(target_os = "macos") {
        "libgeo.dylib"
    } else {
        "libgeo.so"
    });
    ok(&Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", crate::common::workspace())
        .args(["-q", "build", "geo", "--crate-type=cdylib", "-o"])
        .arg(&so)
        .args(crate::common::opt_flags())
        .output()
        .unwrap());
    let exe = dir.join("host");
    let mut cc = Command::new("clang");
    cc.arg("-o")
        .arg(&exe)
        .arg(dir.join("host.c"))
        .arg("-lpthread");
    if cfg!(target_os = "linux") {
        cc.arg("-ldl");
    }
    ok(&cc.output().unwrap());
    let out = Command::new(&exe).arg(&so).output().unwrap();
    ok(&out);
    assert_eq!(
        text(&out.stdout),
        "42 285 3890 1\n16641675000 16641675000\n"
    );
}

/// `--crate-type=staticlib`: one archive with the package, the std it
/// reaches and the runtime, which a C program links alone and calls on
/// its own thread and threads it starts; the header and the module map
/// sit in `include/`.
#[test]
#[cfg(unix)]
fn staticlib_links_alone_from_c() {
    let _ = setup();
    let dir = scratch("staticlib");
    fs::create_dir_all(dir.join("geo/src")).unwrap();
    fs::write(
        dir.join("geo/src/lib.nabi"),
        r#"#[export_name("geo_sum_squares")]
pub extern "C" fn sum_squares(n: i64) -> i64 {
    let xs: List<i64> = [];
    for i in 0..n { xs.push(i * i); }
    let s = 0;
    for x in xs { s += x; }
    s
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("host.c"),
        r#"#include <pthread.h>
#include <stdio.h>
#include "geo.h"
static void *worker(void *arg) {
    long long s = 0;
    for (int i = 0; i < 50; i++) s += geo_sum_squares(1000);
    *(long long *)arg = s;
    return 0;
}
int main(void) {
    printf("%lld\n", (long long)geo_sum_squares(10));
    pthread_t t[4];
    long long r[4];
    for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, worker, &r[i]);
    for (int i = 0; i < 4; i++) pthread_join(t[i], 0);
    printf("%lld %lld\n", r[0], r[3]);
    return 0;
}
"#,
    )
    .unwrap();
    ok(&Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", crate::common::workspace())
        .args(["-q", "build", "geo", "--crate-type=staticlib"])
        .args(crate::common::opt_flags())
        .output()
        .unwrap());
    let built = std::fs::read_dir(dir.join("geo/target"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("static/libgeo.a").is_file())
        .expect("static/libgeo.a");
    let map = fs::read_to_string(built.join("include/module.modulemap")).unwrap();
    assert!(
        map.starts_with("module geo {\n    header \"geo.h\"\n    link \"geo\"\n"),
        "{map}"
    );
    let exe = dir.join("host");
    let mut cc = Command::new("clang");
    cc.arg("-o")
        .arg(&exe)
        .arg("-I")
        .arg(built.join("include"))
        .arg(dir.join("host.c"))
        .arg(built.join("static/libgeo.a"))
        .arg("-lpthread");
    if cfg!(target_os = "linux") {
        cc.args(["-ldl", "-lm"]);
    }
    ok(&cc.output().unwrap());
    let out = Command::new(&exe).output().unwrap();
    ok(&out);
    assert_eq!(text(&out.stdout), "285\n16641675000 16641675000\n");
}

/// `--emit=c-header` and a cdylib's `include/<name>.h`: a C program
/// built against the header and the library (by the system `cc`) gets
/// the same structs, enum values and results; `--lib-version` sets the
/// soname (with its link) or the Mach-O versions; Python's ctypes loads
/// the library too.
#[cfg(unix)]
#[test]
fn cdylib_header_versions_and_python() {
    let _ = setup();
    let host = nabic_target::TargetSpec::host();
    if !nabild::links_shared(host) || Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let apple = cfg!(target_os = "macos");
    let dir = scratch("cdylibheader");
    fs::create_dir_all(dir.join("geo/src")).unwrap();
    fs::write(
        dir.join("geo/src/lib.nabi"),
        r#"use bare::ptr::{ConstPtr, CPtr, Ptr};

/// A point on the plane.
#[repr(C)]
pub struct Point {
    x: f64;
    y: f64;
}

#[repr(C)]
pub struct Rect {
    min: Point;
    max: Point;
    tags: [u8; 4];
}

#[repr(i32)]
pub enum Shape {
    Square = 1,
    WideRect = 7,
}

/// The area of `r`.
///
/// Corners in any order.
#[export_name("geo_area")]
pub extern "C" fn area(r: Rect) -> f64 {
    let w = r.max.x - r.min.x;
    let h = r.max.y - r.min.y;
    (if w < 0.0 { -w } else { w }) * (if h < 0.0 { -h } else { h })
}

#[export_name("geo_shape")]
pub extern "C" fn shape(r: ConstPtr<Rect>) -> Shape {
    let r = unsafe { r.read() };
    if r.max.x - r.min.x == r.max.y - r.min.y { Shape::Square } else { Shape::WideRect }
}

#[export_name("geo_scale")]
pub extern "C" fn scale(p: Ptr<Point>, k: f64) {
    let q = unsafe { p.read() };
    unsafe { p.write(Point { x: q.x * k, y: q.y * k }) };
}

#[export_name("geo_is_null")]
pub extern "C" fn is_null(p: CPtr<Point>) -> bool { p.is_null() }

#[export_name("geo_apply")]
pub extern "C" fn apply(f: extern "C" fn(i32) -> i32, v: i32) -> i32 { f(v) + 1 }

pub fn hidden() -> i64 { 7 }
"#,
    )
    .unwrap();
    let ext = if apple { "dylib" } else { "so" };
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let so = out.join(format!("libgeo.{ext}"));
    ok(&Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", crate::common::workspace())
        .args([
            "-q",
            "build",
            "geo",
            "--crate-type=cdylib",
            "--lib-version=0.3.1",
            "-o",
        ])
        .arg(&so)
        .args(crate::common::opt_flags())
        .output()
        .unwrap());
    let header = dir.join("geo/target/debug/include/geo.h");
    let header = if header.is_file() {
        header
    } else {
        dir.join("geo/target/release/include/geo.h")
    };
    let h = fs::read_to_string(&header).unwrap();
    for want in [
        "#pragma once",
        "extern \"C\" {",
        "/**\n * A point on the plane.\n */\ntypedef struct Point {\n    double x;\n    double y;\n} Point;",
        "    uint8_t tags[4];",
        "typedef int32_t Shape;\n#define SHAPE_SQUARE ((Shape)1)\n#define SHAPE_WIDE_RECT ((Shape)7)",
        " * The area of `r`.\n *\n * Corners in any order.\n */\ndouble geo_area(Rect r);",
        "Shape geo_shape(const Rect *r);",
        "void geo_scale(Point *p, double k);",
        "bool geo_is_null(Point *p /* nullable */);",
        "int32_t geo_apply(int32_t (*f)(int32_t), int32_t v);",
    ] {
        assert!(h.contains(want), "{want}\n---\n{h}");
    }
    assert!(!h.contains("hidden"), "{h}");
    // `--emit=c-header` writes the same text.
    let emitted = out.join("geo.h");
    ok(&Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", crate::common::workspace())
        .args(["-q", "build", "geo", "--emit=c-header", "-o"])
        .arg(&emitted)
        .output()
        .unwrap());
    assert_eq!(fs::read_to_string(&emitted).unwrap(), h);
    fs::write(
        dir.join("main.c"),
        r#"#include <stdio.h>
#include "geo.h"
static int32_t twice(int32_t v) { return v * 2; }
int main(void) {
    Rect r = { { 0, 0 }, { 3, 2 }, { 1, 2, 3, 4 } };
    Point p = { 1.5, -2 };
    geo_scale(&p, 2);
    printf("%g %d %d %g %g %d %d\n", geo_area(r), geo_shape(&r) == SHAPE_WIDE_RECT,
           (int)sizeof(Rect), p.x, p.y, geo_apply(twice, 20), geo_is_null(NULL));
    return 0;
}
"#,
    )
    .unwrap();
    let exe = dir.join("main");
    ok(&Command::new("cc")
        .arg("-Wall")
        .arg("-Werror")
        .arg("-I")
        .arg(header.parent().unwrap())
        .arg(dir.join("main.c"))
        .arg(format!("-L{}", out.display()))
        .args(["-lgeo", "-o"])
        .arg(&exe)
        .output()
        .unwrap());
    let run = Command::new(&exe)
        .env("LD_LIBRARY_PATH", &out)
        .env("DYLD_LIBRARY_PATH", &out)
        .output()
        .unwrap();
    ok(&run);
    assert_eq!(text(&run.stdout), "6 1 40 3 -4 41 1\n");
    if apple {
        let l = Command::new("otool").arg("-L").arg(&so).output().unwrap();
        assert!(
            text(&l.stdout).contains("compatibility version 0.3.0, current version 0.3.1"),
            "{}",
            text(&l.stdout)
        );
    } else {
        // The soname, and a link of that name beside the library.
        let b = fs::read(&so).unwrap();
        assert!(b.windows(12).any(|w| w == b"libgeo.so.0."), "no soname");
        assert!(out.join("libgeo.so.0.3").is_file());
    }
    if Command::new("python3").arg("--version").output().is_ok() {
        let py = Command::new("python3")
            .arg("-c")
            .arg(
                "import ctypes, sys\n\
                 l = ctypes.CDLL(sys.argv[1])\n\
                 class P(ctypes.Structure):\n    _fields_ = [('x', ctypes.c_double), ('y', ctypes.c_double)]\n\
                 p = P(2.0, 5.0)\n\
                 l.geo_scale(ctypes.byref(p), ctypes.c_double(3.0))\n\
                 print(p.x, p.y)\n",
            )
            .arg(&so)
            .output()
            .unwrap();
        ok(&py);
        assert_eq!(text(&py.stdout), "6.0 15.0\n");
    }
}

/// A shared library exports for C only `extern "C"` functions: an
/// `#[export_name]` function of the nabi convention is an error that
/// says what to write.
#[test]
fn cdylib_refuses_nabi_exports() {
    let _ = setup();
    let host = nabic_target::TargetSpec::host();
    if !nabild::links_shared(host) {
        return;
    }
    let dir = scratch("cdylibnabi");
    fs::create_dir_all(dir.join("geo/src")).unwrap();
    fs::write(
        dir.join("geo/src/lib.nabi"),
        "#[export_name(\"geo_twice\")]\npub fn twice(x: i32) -> i32 { x * 2 }\n",
    )
    .unwrap();
    let out = Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", crate::common::workspace())
        .args(["-q", "build", "geo", "--crate-type=cdylib", "-o"])
        .arg(dir.join("libgeo.so"))
        .output()
        .unwrap();
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains(
            "`twice` is exported to C but uses the Nabi ABI: write `pub extern \"C\" fn twice(..)`"
        ),
        "{err}"
    );
}
