//! `Dep::system`, `Dep::framework`, `lib: { kind: "cdylib" }` and `c`.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// zlib and sqlite3 found where the machine has them (pkg-config `.pc`
/// files, a Homebrew keg, the macOS SDK), linked and called; a build
/// without them says how to install them.
#[cfg(unix)]
#[test]
fn system_libraries_link() {
    let root = scratch("system");
    let home = root.join("home");
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    version: \"0.1.0\",\n    native: {\n        \
         \"z\": Dep::system(pkg_config: \"zlib\", brew: \"zlib\", apt: \"zlib1g-dev\"),\n        \
         \"sqlite3\": Dep::system(brew: \"sqlite\", apt: \"libsqlite3-dev\"),\n    },\n}\n",
    );
    write(
        &app.join("src/main.nabi"),
        "extern \"C\" {\n    fn crc32(crc: u64, buf: str, len: u32) -> u64;\n    \
         fn sqlite3_libversion_number() -> i32;\n}\n\n\
         fn main() {\n    let c = unsafe { crc32(0, \"abc\", 3) };\n    \
         let v = unsafe { sqlite3_libversion_number() };\n    println(f\"{c} {v >= 3000000}\");\n}\n",
    );
    let out = nabi(&home, &app, &["run", "-q"]);
    if text(&out.stderr).contains("not found for") {
        eprintln!("skipped: {}", text(&out.stderr));
        return;
    }
    assert_eq!(ok(&out), "891568578 true\n");

    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    native: { \"nope\": Dep::system(pkg_config: \
         \"nabi-no-such-lib\", apt: \"libnope-dev\", brew: \"nope\") },\n}\n",
    );
    let out = nabi(&home, &app, &["build", "-q"]);
    fails(
        &out,
        "system library `nope` (pkg-config `nabi-no-such-lib`) not found for",
    );
    fails(
        &out,
        if cfg!(target_os = "macos") {
            "`brew install nope`"
        } else {
            "`sudo apt install libnope-dev`"
        },
    );
}

/// `#[link(.., kind = "framework")]` and `Dep::framework()`: plain C
/// calls into CoreFoundation and Foundation.
#[cfg(target_os = "macos")]
#[test]
fn frameworks_link() {
    let root = scratch("frameworks");
    let home = root.join("home");
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    native: { \"Foundation\": Dep::framework() },\n}\n",
    );
    write(
        &app.join("src/main.nabi"),
        "#[link(name = \"CoreFoundation\", kind = \"framework\")]\nextern \"C\" {\n    \
         fn CFStringCreateWithCString(alloc: usize, s: str, encoding: u32) -> usize;\n    \
         fn CFStringGetLength(s: usize) -> isize;\n    fn CFRelease(p: usize);\n}\n\n\
         extern \"C\" {\n    fn NSPageSize() -> usize;\n}\n\n\
         fn main() {\n    let s = unsafe { CFStringCreateWithCString(0, \"hello, nabi\", 0x0800_0100) };\n    \
         let n = unsafe { CFStringGetLength(s) };\n    unsafe { CFRelease(s) };\n    \
         let page = unsafe { NSPageSize() };\n    println(f\"{n} {page > 0}\");\n}\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["run", "-q"])), "11 true\n");
}

/// `lib: { kind: "cdylib" }`: `nabi build` makes the shared library at
/// the package's version, and its header.
#[cfg(unix)]
#[test]
fn cdylib_from_the_manifest() {
    let host = nabic_target::TargetSpec::host();
    if !nabild::links_shared(host) {
        return;
    }
    let root = scratch("cdylibkind");
    let home = root.join("home");
    let geo = root.join("geo");
    write(
        &geo.join("nabi.nabion"),
        "Package {\n    name: \"geo\",\n    version: \"1.2.0\",\n    lib: { kind: \"cdylib\" },\n}\n",
    );
    write(
        &geo.join("src/lib.nabi"),
        "#[export_name(\"geo_add\")]\npub extern \"C\" fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    ok(&nabi(&home, &geo, &["build", "-q"]));
    let dir = geo.join("target/debug");
    let h = read(&dir.join("include/geo.h"));
    assert!(h.contains("int64_t geo_add(int64_t a, int64_t b);"), "{h}");
    if cfg!(target_os = "macos") {
        assert!(dir.join("libgeo.dylib").is_file());
    } else {
        assert!(dir.join("libgeo.so").is_file());
        assert!(dir.join("libgeo.so.1").is_file());
    }

    write(
        &geo.join("nabi.nabion"),
        "Package {\n    name: \"geo\",\n    lib: { kind: \"shared\" },\n}\n",
    );
    fails(
        &nabi(&home, &geo, &["build", "-q"]),
        "there is no library kind `shared`; write \"nabi\" (the default), \"cdylib\"",
    );
}

/// `c: { sources }`: nabicc compiles them, the package links them, and
/// a changed source builds again.
#[test]
fn c_sources() {
    let root = scratch("csources");
    let home = root.join("home");
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    c: { sources: [\"c/add.c\"], define: { \"K\": \"2\" }, \
         std: \"c17\" },\n}\n",
    );
    write(&app.join("c/add.c"), "int add_k(int a) { return a + K; }\n");
    write(
        &app.join("src/main.nabi"),
        "extern \"C\" {\n    fn add_k(a: i32) -> i32;\n}\n\nfn main() {\n    \
         let r = unsafe { add_k(40) };\n    println(f\"{r}\");\n}\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["run", "-q"])), "42\n");
    // A changed source compiles again; nabic relinks.
    write(
        &app.join("c/add.c"),
        "int add_k(int a) { return a + K + 1; }\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["run", "-q"])), "43\n");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    c: { sources: [\"c/add.c\"], flags: [\"-Xclang\"] },\n}\n",
    );
    fails(
        &nabi(&home, &app, &["build", "-q"]),
        "`-Xclang` is not a flag `c.flags` takes",
    );
}

/// A cdylib whose C reads a thread-local it declares `extern` (defined
/// by clang's object beside it), `dlopen`ed and called from two
/// threads: each sees its own value. On Android the library reaches it
/// through the loader (`dlopen` refuses initial exec there); a program
/// with the same C linked in takes the relaxed form. With
/// `NABI_TEST_TARGET`, the program runs under `NABI_TEST_RUNNER`.
#[cfg(unix)]
#[test]
fn cdylib_c_thread_local() {
    let target = crate::tools::test_target();
    let t = nabic_target::TargetSpec::resolve_any(target.as_deref()).unwrap();
    if t.object_format != nabic_target::ObjectFormat::Elf || !nabild::links_shared(&t) {
        return;
    }
    let root = scratch("cdylibtls");
    let home = root.join("home");
    let def = root.join("def");
    write(&def.join("def.c"), "__thread long tl_counter;\n");
    let mut cc = Command::new("clang");
    if target.is_some() {
        cc.arg(format!("--target={}", t.c_triple()));
    }
    let Ok(out) = cc
        .args(["-fPIC", "-c", "-o"])
        .arg(def.join("def.o"))
        .arg(def.join("def.c"))
        .output()
    else {
        eprintln!("skipped: no clang");
        return;
    };
    assert!(out.status.success(), "{}", text(&out.stderr));
    let ar = nabild::archive::write(
        nabild::archive::Kind::Gnu,
        &[("def.o".to_owned(), fs::read(def.join("def.o")).unwrap())],
    );
    fs::write(def.join("libtldef.a"), ar).unwrap();
    let bump = "extern _Thread_local long tl_counter;\n\n\
                long tl_bump(long by) { tl_counter += by; return tl_counter; }\n";
    let tl = root.join("tl");
    write(
        &tl.join("nabi.nabion"),
        "Package {\n    name: \"tl\",\n    lib: { kind: \"cdylib\" },\n    \
         c: { sources: [\"c/tl.c\"] },\n}\n",
    );
    write(&tl.join("c/tl.c"), bump);
    write(
        &tl.join("src/lib.nabi"),
        "#[link(\"tldef\")]\nextern \"C\" {\n    fn tl_bump(by: i64) -> i64;\n}\n\n\
         #[export_name(\"tl_add\")]\npub extern \"C\" fn add(by: i64) -> i64 { unsafe { tl_bump(by) } }\n",
    );
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    c: { sources: [\"c/tl.c\"] },\n}\n",
    );
    write(&app.join("c/tl.c"), bump);
    let main = r#"use bare::ffi::Library;
use std::thread;

#[link("tldef")]
extern "C" {
    fn tl_bump(by: i64) -> i64;
}

fn add(by: i64) -> i64 {
    let at = try! std::env::current_exe();
    let so = (at.parent() ?? panic("no directory")).join("SO");
    let lib = try! Library::open(so.as_str());
    let f = try! unsafe { lib.symbol::<extern "C" fn(i64) -> i64>("tl_add") };
    unsafe { f(by) }
}

fn main() {
    let m = add(1);
    let h = thread::spawn(|| (add(5), add(5)));
    let (a, b) = try! h.join();
    println(f"dlopen main {m} {add(1)} thread {a} {b}");
    let m = unsafe { tl_bump(1) };
    let h = thread::spawn(|| (unsafe { tl_bump(5) }, unsafe { tl_bump(5) }));
    let (a, b) = try! h.join();
    println(f"linked main {m} {unsafe { tl_bump(1) }} thread {a} {b}");
}
"#;
    write(
        &app.join("src/main.nabi"),
        &main.replace("SO", &t.dylib("tl")),
    );
    let tflag: Vec<String> = target.iter().map(|t| format!("--target={t}")).collect();
    let build = |dir: &Path| {
        let mut args = vec!["build", "-q"];
        args.extend(tflag.iter().map(String::as_str));
        ok(&nabi_cmd(&home, dir, &args)
            .env("LIBRARY_PATH", &def)
            .output()
            .unwrap());
    };
    build(&tl);
    build(&app);
    let sub = target
        .as_deref()
        .map_or_else(|| "debug".to_owned(), |t| format!("{t}/debug"));
    let bin = app.join("target").join(&sub);
    fs::copy(
        tl.join("target").join(&sub).join(t.dylib("tl")),
        bin.join(t.dylib("tl")),
    )
    .unwrap();
    let exe = bin.join("app");
    let runner = std::env::var("NABI_TEST_RUNNER").unwrap_or_default();
    let words: Vec<&str> = runner.split_whitespace().collect();
    let out = match (target.is_some(), words.split_first()) {
        (true, Some((r, rest))) => Command::new(r).args(rest).arg(&exe).output(),
        _ => Command::new(&exe).output(),
    }
    .unwrap();
    assert_eq!(
        ok(&out),
        "dlopen main 1 2 thread 5 10\nlinked main 1 2 thread 5 10\n"
    );
}
