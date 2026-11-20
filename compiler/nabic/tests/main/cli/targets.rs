//! Cross targets, the sysroot and target options.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

/// nabic's library and runtime are under its own sysroot (`bin/..`),
/// whatever `NABI_HOME` says; a missing one is named.
#[test]
fn sysroot_is_beside_nabic() {
    let dir = scratch("sysroot");
    let sys = dir.join("sys");
    fs::create_dir_all(sys.join("bin")).unwrap();
    let nabic_exe = sys
        .join("bin")
        .join(format!("nabic{}", std::env::consts::EXE_SUFFIX));
    fs::copy(NABIC, &nabic_exe).unwrap();
    put(sys.join("lib/nabi/core/src/lib.nabi"), "fn (\n").unwrap();
    put(dir.join("x.nabi"), "let x = 1;\n").unwrap();
    let run = |args: &[&str]| {
        Command::new(&nabic_exe)
            .current_dir(&dir)
            .env("NABI_HOME", workspace())
            .args(args)
            .output()
            .unwrap()
    };
    // Paths as `/`-separated, whatever the platform's separator.
    let stderr = |o: &std::process::Output| text(&o.stderr).replace('\\', "/");
    let out = run(&["check", "x.nabi"]);
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("sys/lib/nabi/core/src/lib.nabi"));
    put(sys.join("lib/nabi/core/src/lib.nabi"), "").unwrap();
    let out = run(&["build", "x.nabi"]);
    assert_exit(&out, 1);
    let err = stderr(&out);
    let rt = format!("sys/lib/{}", nabic_target::TargetSpec::host().runtime_lib());
    assert!(
        err.contains("the runtime library is missing: no ") && err.contains(&rt),
        "{err}"
    );
    // A copy of nabic is hundreds of MB.
    let _ = fs::remove_dir_all(&dir);
}

/// `LC_BUILD_VERSION`'s platform and minimum OS of Mach-O object `o`.
fn macho_build_version(o: &[u8]) -> Option<(u32, u32)> {
    let u = |at: usize| u32::from_le_bytes(o[at..at + 4].try_into().unwrap());
    let mut at = 32;
    for _ in 0..u(16) {
        if u(at) == 0x32 {
            return Some((u(at + 8), u(at + 12)));
        }
        at += u(at + 4) as usize;
    }
    None
}

/// `--target` links what the sysroot ships; a target without a runtime
/// says so, one that is not built in (`sparc64-unknown-linux-gnu`) is unknown.
#[test]
fn cross_targets_link_from_the_sysroot() {
    let dir = scratch("cross");
    put(
        dir.join("h.nabi"),
        "fn main() -> i32 {\n    println(\"ok\");\n    0\n}\n",
    )
    .unwrap();
    // Not built by default: no runtime, unless `targets/build.sh` made one.
    let s390x = "s390x-unknown-linux-gnu";
    let built = Path::new(NABIC)
        .parent()
        .unwrap()
        .join("../lib/nabi/targets")
        .join(s390x)
        .join("libnabir.a")
        .is_file();
    if !built {
        let out = nabic(None, &dir, &["build", "h.nabi", "--target", s390x]);
        assert_exit(&out, 1);
        let err = text(&out.stderr);
        assert!(err.contains(&format!("no runtime for {s390x}")), "{err}");
    }
    // The objects need no runtime: ELF of the target's machine and byte
    // order.
    for (t, machine, data) in [
        ("powerpc64le-unknown-linux-gnu", [21, 0], 1),
        (s390x, [0, 22], 2),
    ] {
        let out = nabic(
            None,
            &dir,
            &["build", "h.nabi", "--emit=obj", "--target", t, "-o", "h.o"],
        );
        assert_exit(&out, 0);
        let o = std::fs::read(dir.join("h.o")).unwrap();
        assert_eq!(
            (&o[..4], o[5], [o[18], o[19]]),
            (&b"\x7fELF"[..], data, machine),
            "{t}"
        );
    }
    // Mach-O objects name their platform and minimum OS, as Apple's
    // linkers check: iOS 13.4 on the device and the simulator.
    for (t, platform) in [
        ("aarch64-apple-darwin", 1),
        ("aarch64-apple-ios", 2),
        ("aarch64-apple-ios-sim", 7),
    ] {
        let out = nabic(
            None,
            &dir,
            &["build", "h.nabi", "--emit=obj", "--target", t, "-o", "h.o"],
        );
        assert_exit(&out, 0);
        let o = std::fs::read(dir.join("h.o")).unwrap();
        let minos = if platform == 1 {
            0x000b_0000
        } else {
            0x000d_0400
        };
        assert_eq!(macho_build_version(&o), Some((platform, minos)), "{t}");
    }
    let out = nabic(
        None,
        &dir,
        &[
            "build",
            "h.nabi",
            "--emit=obj",
            "--target",
            "wasm32-wasip2",
            "-o",
            "h.o",
        ],
    );
    assert_exit(&out, 0);
    let o = std::fs::read(dir.join("h.o")).unwrap();
    assert_eq!(&o[..8], b"\0asm\x01\0\0\0", "wasm32-wasip2");
    // The code as text: each function's instructions as objdump shows
    // them (`listing`), and the object as assembly source (`asm`), on
    // every ISA.
    for (t, ret, main) in [
        (
            "x86_64-unknown-linux-gnu",
            "\tret\n",
            "\t.type\tmain,@function\n",
        ),
        ("aarch64-apple-darwin", "\tret\n", "\t.globl\t_main\n"),
        (
            "aarch64-apple-ios-sim",
            "\tret\n",
            "\t.build_version\tiossimulator, 13, 4\n",
        ),
        ("i686-unknown-linux-gnu", "\tret\n", "\t.globl\tmain\n"),
        ("i686-pc-windows-msvc", "\tret\n", "\t.def\t_nabi_main;\n"),
        (s390x, "\tbr\t%r14\n", "\t.type\tmain,@function\n"),
        ("wasm32-wasip2", "\treturn\n", "\tend_function\n"),
        (
            "armv7-unknown-linux-gnueabihf",
            "\tpop\t{r4, r5, r6, r7, r8, r9, r10, r11, pc}\n",
            "\t.type\tmain,%function\n",
        ),
        (
            "riscv64gc-unknown-linux-gnu",
            "\tret\n",
            "\t.option\tnorvc\n",
        ),
        (
            "riscv32imac-unknown-none-elf",
            "\tret\n",
            "\t.option\tnorelax\n",
        ),
        (
            "powerpc64le-unknown-linux-gnu",
            "\tblr\n",
            "\t.localentry\tmain, 8\n",
        ),
    ] {
        let args = [
            "build",
            "h.nabi",
            "--emit=listing",
            "--target",
            t,
            "-o",
            "h.lst",
        ];
        assert_exit(&nabic(None, &dir, &args), 0);
        let s = std::fs::read_to_string(dir.join("h.lst")).unwrap();
        assert!(s.starts_with("\n<") && s.contains(ret), "{t}: {s}");
        let args = ["build", "h.nabi", "--emit=asm", "--target", t, "-o", "h.s"];
        let out = nabic(None, &dir, &args);
        assert_exit(&out, 0);
        let s = std::fs::read_to_string(dir.join("h.s")).unwrap();
        assert!(
            s.contains(ret) && s.contains(main) && !s.contains("\n<"),
            "{t}: {s}"
        );
    }
    // The source assembles, where a clang for the target is at hand.
    for (t, clang, feats) in [
        ("x86_64-unknown-linux-gnu", "x86_64-linux-gnu", &[][..]),
        ("aarch64-apple-darwin", "arm64-apple-macos11", &[]),
        ("aarch64-apple-ios", "arm64-apple-ios13.4", &[]),
        (
            "aarch64-apple-ios-sim",
            "arm64-apple-ios13.4-simulator",
            &[],
        ),
        (
            "armv7-unknown-linux-gnueabihf",
            "armv7-linux-gnueabihf",
            &[],
        ),
        ("wasm32-wasip2", "wasm32-wasi", &["-msimd128"]),
    ] {
        let args = ["build", "h.nabi", "--emit=asm", "--target", t, "-o", "h.s"];
        assert_exit(&nabic(None, &dir, &args), 0);
        let ok = Command::new("clang")
            .arg(format!("--target={clang}"))
            .args(feats)
            .args(["-c", "-x", "assembler", "h.s", "-o", "h.re.o"])
            .current_dir(&dir)
            .output();
        match ok {
            Ok(o) if o.status.success() => {}
            // A clang built without the target (FreeBSD's base has no wasm32).
            Ok(o)
                if text(&o.stderr).contains("No available targets")
                    || text(&o.stderr).contains("unknown target triple") => {}
            Ok(o) => panic!("{t}: {}", text(&o.stderr)),
            Err(_) => eprintln!("no clang: {t} not assembled"),
        }
    }
    let out = nabic(
        None,
        &dir,
        &["build", "h.nabi", "--target", "sparc64-unknown-linux-gnu"],
    );
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains("unknown target `sparc64-unknown-linux-gnu`"),
        "{err}"
    );

    let _ = setup();
    let targets = Path::new(NABIC)
        .parent()
        .unwrap()
        .join("../lib/nabi/targets");
    if !targets.is_dir() {
        eprintln!("skipped: no {} (targets/build.sh)", targets.display());
        return;
    }
    for t in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "armv7-unknown-linux-gnueabihf",
        "armv7-unknown-linux-musleabihf",
        "riscv64gc-unknown-linux-gnu",
        "powerpc64le-unknown-linux-gnu",
        "s390x-unknown-linux-gnu",
        "x86_64-unknown-freebsd",
    ] {
        if !targets.join(t).is_dir() {
            continue;
        }
        let spec = TargetSpec::builtin(t).unwrap();
        if spec.libc == Libc::Glibc {
            check_glibc_runtime(
                &spec,
                &fs::read(targets.join(t).join("libnabir.a")).unwrap(),
            );
        }
        for o in ["-O0", "-O2"] {
            let exe = dir.join(format!("h-{t}{o}"));
            let exe_s = exe.display().to_string();
            let out = nabic(
                None,
                &dir,
                &["-q", "build", "h.nabi", "--target", t, o, "-o", &exe_s],
            );
            assert_exit(&out, 0);
            let b = fs::read(&exe).unwrap();
            if spec.os == "freebsd" {
                check_freebsd_elf(&spec, &b);
            } else {
                check_linux_elf(&spec, &b);
            }
        }
    }
}

/// A browser module is a reactor; its exports stay callable from JS.
#[test]
fn browser_module_exports_to_js() {
    let dir = scratch("browser-exports");
    let _ = setup();
    let t = Path::new(NABIC)
        .parent()
        .unwrap()
        .join("../lib/nabi/targets/wasm32-unknown-unknown");
    if !t.is_dir() {
        eprintln!("skipped: no {} (targets/build.sh)", t.display());
        return;
    }
    put(
        dir.join("app.nabi"),
        "extern \"C\" { fn nabi_gc_collect(); }\n\
         \n\
         #[export_name(\"add\")]\n\
         extern \"C\" fn add(a: i32, b: i32) -> i32 { a + b }\n\
         \n\
         #[export_name(\"strings\")]\n\
         extern \"C\" fn strings(n: i32) -> i64 {\n\
         \x20   let xs: List<str> = List::new();\n\
         \x20   let i = 0;\n\
         \x20   while i < n {\n\
         \x20       xs.push(f\"s{i}\");\n\
         \x20       if i % 1000 == 0 { unsafe { nabi_gc_collect() }; }\n\
         \x20       i += 1;\n\
         \x20   }\n\
         \x20   println(f\"{xs.len()} strings, last {xs[xs.len() - 1]}\");\n\
         \x20   let total: i64 = 0;\n\
         \x20   for s in xs { total += s.len() as i64; }\n\
         \x20   total\n\
         }\n\
         \n\
         fn main() -> i32 {\n\
         \x20   println(\"main\");\n\
         \x20   3\n\
         }\n",
    )
    .unwrap();
    let script = format!(
        "import {{ load }} from {:?}; import {{ readFileSync }} from 'node:fs'; \
         const app = await load(readFileSync(process.argv[1])); \
         console.log('code', app.main()); console.log('add', app.exports.add(2, 3)); \
         console.log('total', app.exports.strings(20000) === 108890n);",
        t.join("nabi.mjs").display().to_string()
    );
    for o in ["-O0", "-O2"] {
        let out = nabic(
            None,
            &dir,
            &[
                "-q",
                "build",
                "app.nabi",
                "--target",
                "wasm32-unknown-unknown",
                o,
                "-o",
                "app.wasm",
            ],
        );
        assert_exit(&out, 0);
        let Ok(run) = Command::new("node")
            .args(["--input-type=module", "-e", &script, "app.wasm"])
            .current_dir(&dir)
            .output()
        else {
            eprintln!("skipped: no node");
            return;
        };
        assert_exit(&run, 0);
        assert_eq!(
            text(&run.stdout),
            "main\ncode 3\nadd 5\n20000 strings, last s19999\ntotal true\n",
            "{o}"
        );
    }
}

/// A Linux executable for `t`: machine, class, static (musl) or a PIE
/// with glibc's loader and versions up to 2.34.
pub(crate) fn check_linux_elf(spec: &TargetSpec, b: &[u8]) {
    let t = spec.name.as_str();
    let machine: u16 = match spec.arch.as_str() {
        "x86_64" => 62,
        "aarch64" => 183,
        "x86" => 3,
        "riscv64" => 243,
        "powerpc64" => 21,
        "s390x" => 22,
        _ => 40,
    };
    let class = if spec.pointer_width == 64 { 2 } else { 1 };
    let be = spec.endian == "big";
    let u16_at = |at: usize| {
        let v = [b[at], b[at + 1]];
        if be {
            u16::from_be_bytes(v)
        } else {
            u16::from_le_bytes(v)
        }
    };
    let loader = spec.dynamic_loader.as_bytes();
    let has = |s: &[u8]| b.windows(s.len()).any(|w| w == s);
    assert_eq!(&b[..4], b"\x7fELF", "{t}");
    assert_eq!(b[4], class, "{t}: ELF class");
    assert_eq!(b[5], if be { 2 } else { 1 }, "{t}: byte order");
    assert_eq!(u16_at(18), machine, "{t}");
    if spec.arch == "powerpc64" {
        // ELFv2.
        assert_eq!(b[48], 0x2, "{t}: e_flags");
    }
    if spec.arch == "riscv64" {
        // RVC, the double-float ABI (LP64D).
        assert_eq!(b[48], 0x5, "{t}: e_flags");
    }
    if spec.arch == "arm" {
        // EABI version 5, hard-float.
        assert_eq!(&b[36..40], &0x0500_0400u32.to_le_bytes(), "{t}: e_flags");
    }
    let ty = u16_at(16);
    if spec.dynamic_linking {
        assert_eq!(ty, 3, "{t}: a PIE");
        assert!(
            has(loader) && has(b"libc.so.6") && has(b"GLIBC_2.34"),
            "{t}"
        );
        assert!(!has(b"GLIBC_2.35"), "{t}: newer than glibc 2.34");
    } else {
        assert_eq!(ty, 2, "{t}: a static ET_EXEC");
        assert!(!has(b"ld-linux"), "{t}");
    }
}

/// A FreeBSD PIE: its OS/ABI, loader, libc and ABI note.
pub(crate) fn check_freebsd_elf(spec: &TargetSpec, b: &[u8]) {
    let t = spec.name.as_str();
    let has = |s: &[u8]| b.windows(s.len()).any(|w| w == s);
    assert_eq!(&b[..4], b"\x7fELF", "{t}");
    assert_eq!(b[7], 9, "{t}: ELFOSABI_FREEBSD");
    assert_eq!(u16::from_le_bytes([b[16], b[17]]), 3, "{t}: a PIE");
    assert_eq!(u16::from_le_bytes([b[18], b[19]]), 62, "{t}");
    assert!(
        has(b"/libexec/ld-elf.so.1\0") && has(b"libc.so.7\0") && has(b"FBSD_1.0\0"),
        "{t}"
    );
    assert!(has(b"FreeBSD\0"), "{t}: the ABI note");
}

/// A glibc runtime archive: on 32-bit targets it calls the 64-bit time
/// and file-offset interfaces (glibc 2.34+), none of the 32-bit `time_t`
/// / `off_t` ones (no 2038 limit).
pub(crate) fn check_glibc_runtime(spec: &TargetSpec, a: &[u8]) {
    if spec.pointer_width == 64 {
        return;
    }
    let t = spec.name.as_str();
    // A symbol name in an object's string table.
    let has = |s: &str| {
        let n = s.len();
        a.windows(n + 2)
            .any(|w| w[0] == 0 && w[n + 1] == 0 && &w[1..=n] == s.as_bytes())
    };
    for old in [
        "clock_gettime",
        "clock_getres",
        "clock_nanosleep",
        "nanosleep",
        "gettimeofday",
        "fstat",
        "fstatat",
        "fstat64",
        "fstatat64",
        "lstat",
        "lstat64",
        "pthread_cond_timedwait",
        "setsockopt",
        "getsockopt",
        "sendmsg",
        "recvmsg",
        "fcntl",
        "futimens",
        "utimensat",
        "ppoll",
        "pselect",
        "timerfd_settime",
        "timerfd_gettime",
        "lseek",
        "readdir",
        "mmap",
        "openat",
        "ftruncate",
        "sendfile",
    ] {
        assert!(!has(old), "{t}: the runtime calls the 32-bit `{old}`");
    }
    for new in [
        "__clock_gettime64",
        "__nanosleep64",
        "__fstat64_time64",
        "lseek64",
    ] {
        assert!(has(new), "{t}: the runtime does not call `{new}`");
    }
}

/// `-C target-feature` / `-C target-cpu` are checked and accepted; one
/// that changes no code says so.
#[test]
fn target_cpu_and_features() {
    let env = setup();
    let dir = scratch("target-cpu");
    let src = "fn add(a: List<f32>, b: List<f32>, c: List<f32>) {\n    \
               for i in 0..a.len() {\n        c[i] = a[i] + b[i];\n    }\n}\n\n\
               let a: List<f32> = [];\nlet b: List<f32> = [];\nlet c: List<f32> = [];\n\
               for i in 0..64 {\n    a.push(1.0f32);\n    b.push(2.0f32);\n    \
               c.push(0.0f32);\n}\nadd(a, b, c);\nprintln(f\"{c[3]}\");\n";
    put(dir.join("v.nabi"), src).unwrap();
    let out = build_run(
        Some(&env),
        &dir,
        &["v.nabi", "-O2", "-C", "target-cpu=native"],
    );
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "3\n");
    let out = build_run(Some(&env), &dir, &["v.nabi", "-C", "target-cpu=native"]);
    assert_exit(&out, 0);
    // Apple arm64: the baseline has every feature the backend uses.
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        assert!(text(&out.stderr).contains("-C target-cpu=native changes no code on aarch64"));
    }
    let out = build_run(
        Some(&env),
        &dir,
        &["v.nabi", "-O2", "-C", "target-feature=+sse4.1"],
    );
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "3\n");
    assert!(text(&out.stderr).contains("-C target-feature=+sse4.1 changes no code"));
    let out = nabic(
        None,
        &dir,
        &[
            "build",
            "v.nabi",
            "--target=x86_64-unknown-linux-gnu",
            "-C",
            "target-cpu=haswel",
        ],
    );
    assert_exit(&out, 2);
    assert!(text(&out.stderr).contains("did you mean `haswell`?"));

    let out = nabic(
        None,
        &dir,
        &["build", "v.nabi", "-C", "target-feature=avx2"],
    );
    assert_exit(&out, 2);
    assert!(text(&out.stderr).contains("`avx2` is not `+feature` or `-feature`"));
    let foreign = if cfg!(target_arch = "x86_64") {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let out = nabic(
        None,
        &dir,
        &[
            "build",
            "v.nabi",
            "-O2",
            &format!("--target={foreign}"),
            "-C",
            "target-cpu=native",
        ],
    );
    assert_exit(&out, 2);
    assert!(text(&out.stderr).contains("is not the build machine's platform"));
}

/// `nabic --print cfg`: the target's keys and `--cfg`.
#[test]
fn print_cfg_lists_the_active_cfg() {
    let dir = scratch("print_cfg");
    let out = nabic(
        None,
        &dir,
        &[
            "--print",
            "cfg",
            "--target=x86_64-unknown-linux-gnu",
            "--cfg",
            "feature=gpu",
            "-O2",
        ],
    );
    assert_exit(&out, 0);
    assert_eq!(
        text(&out.stdout),
        "feature=\"gpu\"\ntarget_arch=\"x86_64\"\ntarget_endian=\"little\"\n\
         target_env=\"gnu\"\ntarget_family=\"unix\"\ntarget_os=\"linux\"\n\
         target_pointer_width=\"64\"\n"
    );
    let out = nabic(None, &dir, &["--print", "cfg"]);
    assert!(text(&out.stdout).starts_with("debug_assertions\n"));
    assert_exit(&nabic(None, &dir, &["--print", "sysroot"]), 2);
}

/// `--print target-list` names every built-in target (with the
/// in-progress opt-in); `--print target-spec` prints one as NABION,
/// which reads back as the same spec.
#[test]
fn print_target_list_and_spec() {
    let dir = scratch("print_target");
    let out = nabic(None, &dir, &["--print", "target-list"]);
    assert_exit(&out, 0);
    let list = text(&out.stdout);
    let names: Vec<&str> = list.lines().collect();
    assert_eq!(names, nabic_target::BUILTIN);
    for t in nabic_target::BUILTIN {
        let out = nabic(None, &dir, &["--print", "target-spec", "--target", t]);
        assert_exit(&out, 0);
        let spec = TargetSpec::from_nabion(&text(&out.stdout), "out", t).unwrap();
        assert_eq!(spec, TargetSpec::resolve_any(Some(t)).unwrap(), "{t}");
    }
    let out = nabic(None, &dir, &["--print", "target-spec"]);
    assert_exit(&out, 0);
    assert_eq!(
        text(&out.stdout),
        format!("{}\n", TargetSpec::host().to_nabion())
    );
    let out = nabic(None, &dir, &["--print", "target-spec", "--target", "nope"]);
    assert_exit(&out, 1);
    assert!(text(&out.stderr).contains("unknown target `nope`"));
}

/// `--print lldbinit` / `gdbinit` / `natvis`: what loads the debugger
/// formatters the toolchain ships (a repository build: `library/`).
#[test]
fn print_debugger_formatters() {
    let dir = scratch("print_lldbinit");
    for (what, head, tail, file) in [
        ("lldbinit", "command script import '", "'\n", "lldb/nabi.py"),
        ("gdbinit", "source ", "\n", "gdb/nabi.py"),
        ("natvis", "", "\n", "natvis/nabi.natvis"),
    ] {
        let out = nabic(None, &dir, &["--print", what]);
        assert_exit(&out, 0);
        let line = text(&out.stdout);
        let path = line
            .strip_prefix(head)
            .and_then(|p| p.strip_suffix(tail))
            .unwrap_or_else(|| panic!("{what}: {line:?}"));
        let path = Path::new(path);
        assert!(path.ends_with(file) && path.is_file(), "{what}: {line:?}");
    }
}

/// A custom target (`--target <name>.nabion`): a copy of the host's spec
/// builds the same code under its own name; its runtime is looked up in
/// `lib/nabi/targets/<name>/`; a bad field is an error at its line.
#[test]
fn custom_target_file() {
    let env = setup();
    let dir = scratch("custom_target");
    put(
        dir.join("h.nabi"),
        "fn main() -> i32 {\n    println(\"ok\");\n    0\n}\n",
    )
    .unwrap();
    put(dir.join("mine.nabion"), TargetSpec::host().to_nabion()).unwrap();
    for kind in ["mir", "obj"] {
        let emit = |extra: &[&str], out: &str| {
            let mut args = vec!["-q", "emit", "h.nabi", "--emit", kind, "-o", out];
            args.extend(extra);
            assert_exit(&nabic(Some(&env), &dir, &args), 0);
            fs::read(dir.join(out)).unwrap()
        };
        let host = emit(&[], "host.out");
        let mine = emit(&["--target", "mine.nabion"], "mine.out");
        assert!(host == mine, "{kind} differs");
    }
    // A shipped C library (Linux) is looked up in `targets/mine/` too:
    // link with a sysroot whose `mine` is the host's directory.
    let host = TargetSpec::host();
    let sys = Path::new(NABIC)
        .parent()
        .unwrap()
        .join(format!("custom-target-{}", std::process::id()));
    let exe = if host.libc.shipped() {
        let _ = fs::remove_dir_all(&sys);
        fs::create_dir_all(sys.join("bin")).unwrap();
        fs::create_dir_all(sys.join("lib/nabi/targets")).unwrap();
        let exe = sys.join("bin/nabic");
        // A copy: `current_exe` must name it, and a hard link would change
        // `NABIC`'s ctime, so its identity, under the tests running beside.
        fs::copy(NABIC, &exe).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            nabic_core::real_path(
                Path::new(NABIC)
                    .parent()
                    .unwrap()
                    .join("../lib/nabi/targets"),
            )
            .unwrap()
            .join(&host.name),
            sys.join("lib/nabi/targets/mine"),
        )
        .unwrap();
        exe
    } else {
        PathBuf::from(NABIC)
    };
    let out = Command::new(&exe)
        .current_dir(&dir)
        .env("NABI_HOME", workspace())
        .args([
            "-q",
            "build",
            "h.nabi",
            "--target",
            "mine.nabion",
            "-o",
            "mine",
        ])
        .arg("--nabir")
        .arg(&env.nabir)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&sys);
    assert_exit(&out, 0);
    let run = Command::new(dir.join("mine")).output().unwrap();
    assert_exit(&run, 0);
    assert_eq!(text(&run.stdout), "ok\n");
    // Without --nabir: the runtime of target `mine`.
    let out = nabic(None, &dir, &["build", "h.nabi", "--target", "mine.nabion"]);
    assert_exit(&out, 1);
    let err = text(&out.stderr).replace('\\', "/");
    assert!(
        err.contains("no runtime for mine:") && err.contains("targets/mine"),
        "{err}"
    );
    let bad =
        TargetSpec::host()
            .to_nabion()
            .replacen("TargetSpec {", "TargetSpec {\n    tier: 1,", 1);
    put(dir.join("bad.nabion"), bad).unwrap();
    let out = nabic(
        None,
        &dir,
        &["--print", "target-spec", "--target", "bad.nabion"],
    );
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains("unknown field `tier`") && err.contains("bad.nabion:2:5"),
        "{err}"
    );
}

/// `isize` / `usize` limits follow the target's pointer width: a
/// 32-bit target folds them to 32-bit values.
#[test]
fn pointer_sized_limits_follow_the_target() {
    let env = setup();
    let dir = scratch("pointer_width_limits");
    put(
        dir.join("w.nabi"),
        "#[export_name(\"t_max\")]\npub extern \"C\" fn t_max() -> i64 { usize::MAX as i64 + \
         usize::BITS as i64 + isize::MIN as i64 }\nfn main() {}\n",
    )
    .unwrap();
    for (target, want) in [
        (
            "--target=i686-unknown-linux-gnu",
            ["4294967295usize", "32u32", "-2147483648isize"],
        ),
        (
            "--target=x86_64-unknown-linux-gnu",
            [
                "18446744073709551615usize",
                "64u32",
                "-9223372036854775808isize",
            ],
        ),
    ] {
        let out = nabic(
            Some(&env),
            &dir,
            &["build", "--emit=mir", target, "w.nabi", "-o", "w.mir"],
        );
        assert_exit(&out, 0);
        let mir = fs::read_to_string(dir.join("w.mir")).unwrap();
        for w in want {
            assert!(mir.contains(w), "{target}: no {w}: {mir}");
        }
    }
}

/// `#[cfg]` on an impl (`ChildExt::pidfd`, Linux only):
/// the method exists for a Linux target and not for macOS.
#[test]
fn cfg_on_impl_members_follows_the_target() {
    let dir = scratch("cfg_impl_member");
    put(
        dir.join("p.nabi"),
        "use std::process::Command;\nfn main() {\n    \
         let c = try Command::new(\"true\").spawn() catch _ { return; };\n    \
         let fd = try c.pidfd() catch _ { -1 };\n    println(f\"{fd}\");\n}\n",
    )
    .unwrap();
    let check = |target: &str| nabic(None, &dir, &["check", target, "p.nabi"]);
    assert_exit(&check("--target=x86_64-unknown-linux-gnu"), 0);
    let out = check("--target=aarch64-apple-darwin");
    assert_exit(&out, 1);
    assert!(text(&out.stderr).contains("no method `pidfd`"));
}

/// A native build compiles the standard library functions it reaches at
/// every level; nothing of the library is prebuilt.
#[test]
fn optimized_builds_compile_the_std_they_reach() {
    let env = setup();
    let dir = scratch("o3-std");
    put(dir.join("main.nabi"), "println(\"hi\");\n").unwrap();
    for opt in ["-O2", "-O3"] {
        let out = build_run(Some(&env), &dir, &[opt, "-v", "main.nabi"]);
        assert_exit(&out, 0);
        assert_eq!(text(&out.stdout), "hi\n");
        assert!(
            !text(&out.stderr).contains("Std "),
            "{opt}: {}",
            text(&out.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn nabi_cc_links_wit_components() {
    let _env = setup();
    let rt = workspace().join("target/lib/nabi/targets/wasm32-wasip2/libnabir.a");
    if !rt.is_file() {
        eprintln!("skipped: no wasm32-wasip2 runtime (targets/build.sh wasm32-wasip2)");
        return;
    }
    let dir = scratch("nabi-cc-wit");
    put(
        dir.join("calc.wit"),
        "package test:calc;\n\nworld calc {\n    export add: func(a: s32, b: s32) -> s32;\n}\n",
    )
    .unwrap();
    put(
        dir.join("main.nabi"),
        "ffi::wit::export { \"calc.wit\" }\n\n#[wit::export]\nfn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .unwrap();
    let cc = dir.join("cc.sh");
    let args = dir.join("args");
    put(
        &cc,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 1\n",
            args.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&cc, fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", workspace())
        .env("NABI_CC", &cc)
        .args([
            "build",
            "main.nabi",
            "--target=wasm32-wasip2",
            "-o",
            "out.wasm",
        ])
        .output()
        .unwrap();
    assert_exit(&out, 1);
    let args = fs::read_to_string(&args).unwrap();
    for want in [
        "--target=wasm32-wasip2",
        "-Wl,--export=add",
        "-Wl,--export=cabi_realloc",
        "-mexec-model=reactor",
    ] {
        assert!(args.lines().any(|l| l == want), "{want}:\n{args}");
    }
    assert!(
        args.lines().any(|l| l.starts_with("-Wl,--component-type,")),
        "{args}"
    );
}

/// A near-miss target name (Rust's older spellings) is an unknown target,
/// with the closest built-in one suggested.
#[test]
fn target_old_name_suggests() {
    let dir = scratch("target-old");
    for (old, new) in [
        ("wasm32-wasi", "wasm32-wasip2"),
        ("wasm32-wasi-threads", "wasm32-wasip1-threads"),
        ("riscv32imac-unknown-none", "riscv32imac-unknown-none-elf"),
    ] {
        let out = nabic(None, &dir, &["--print", "target-spec", "--target", old]);
        assert_exit(&out, 1);
        assert!(
            text(&out.stderr).contains(&format!("unknown target `{old}`; did you mean `{new}`?")),
            "{}",
            text(&out.stderr)
        );
    }
}

/// A macOS program links against the default SDK (no SDKROOT) and against
/// every installed one: the macOS 27 SDK's .tbd files list arm64e.x1.
#[test]
fn links_against_every_installed_macos_sdk() {
    if !cfg!(target_os = "macos") || crate::common::test_target().is_some() {
        return;
    }
    let env = setup();
    let dir = scratch("macos_sdks");
    put(dir.join("h.nabi"), "fn main() { println(\"hi\"); }\n").unwrap();
    let mut sdks: Vec<Option<PathBuf>> = vec![None];
    for d in [
        "/Library/Developer/CommandLineTools/SDKs",
        "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs",
    ] {
        let Ok(rd) = fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("MacOSX")
                && !p.is_symlink()
                && p.join("usr/lib/libSystem.tbd").is_file()
            {
                sdks.push(Some(p));
            }
        }
    }
    for sdk in &sdks {
        let exe = dir.join("h");
        let mut cmd = Command::new(NABIC);
        cmd.current_dir(&dir).env("NABI_HOME", workspace());
        match sdk {
            Some(p) => cmd.env("SDKROOT", p),
            None => cmd.env_remove("SDKROOT"),
        };
        let out = cmd
            .args(["-q", "build", "h.nabi", "-o"])
            .arg(&exe)
            .arg("--nabir")
            .arg(&env.nabir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{sdk:?}: {}", text(&out.stderr));
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(text(&run.stdout), "hi\n", "{sdk:?}");
    }
}

/// `nabic <args>` without the in-progress opt-in.
fn nabic_supported(dir: &Path, args: &[&str]) -> Output {
    Command::new(NABIC)
        .current_dir(dir)
        .env("NABI_HOME", workspace())
        .env_remove(nabic_target::IN_PROGRESS_VAR)
        .args(args)
        .output()
        .unwrap()
}

/// An in-progress target is refused without the opt-in: by `build`,
/// `--print target-spec` and a package's target, with what to build now
/// where a supported target stands in. `--print target-list` leaves the
/// in-progress targets out.
#[test]
fn in_progress_targets_need_the_opt_in() {
    let dir = scratch("in_progress");
    put(dir.join("h.nabi"), "fn main() {}\n").unwrap();
    let ios = "nabic: error: target `aarch64-apple-ios` (iOS devices) is in progress: it is not tested on \
               real hardware yet and comes in a later release; for now build for the simulator \
               with `--target aarch64-apple-ios-sim`";
    let android = "nabic: error: target `aarch64-linux-android` (64-bit ARM Android devices) is in \
                   progress: it is not tested on real hardware yet and comes in a later release; \
                   for now build for the emulator with `--target x86_64-linux-android`";
    let riscv = "nabic: error: target `riscv64gc-unknown-linux-gnu` (64-bit RISC-V Linux) is in \
                 progress: it is not tested on real hardware yet and comes in a later release; no \
                 supported target stands in for it yet";
    for (t, want) in [
        ("aarch64-apple-ios", ios),
        ("aarch64-linux-android", android),
        ("riscv64gc-unknown-linux-gnu", riscv),
    ] {
        let out = nabic_supported(&dir, &["build", "h.nabi", "--target", t]);
        assert_exit(&out, 1);
        assert_eq!(text(&out.stderr).trim_end(), want, "{t}");
        let out = nabic_supported(&dir, &["--print", "target-spec", "--target", t]);
        assert_exit(&out, 1);
        assert_eq!(text(&out.stderr).trim_end(), want, "{t}");
    }
    let out = nabic_supported(&dir, &["--print", "target-list"]);
    assert_exit(&out, 0);
    let list = text(&out.stdout);
    let names: Vec<&str> = list.lines().collect();
    let supported: Vec<&str> = nabic_target::BUILTIN
        .iter()
        .copied()
        .filter(|t| !nabic_target::in_progress(t))
        .collect();
    assert_eq!(names, supported);
    assert!(!names.contains(&"s390x-unknown-linux-gnu"), "{list}");
    assert!(names.contains(&"aarch64-apple-ios-sim"), "{list}");
    for arm in [
        "armv7-unknown-linux-gnueabihf",
        "armv7-unknown-linux-musleabihf",
    ] {
        assert!(names.contains(&arm), "{list}");
        let out = nabic_supported(&dir, &["--print", "target-spec", "--target", arm]);
        assert_exit(&out, 0);
    }
}

/// `NABI_IN_PROGRESS_TARGETS=1`, the maintainers' switch, lets `--target`
/// take an in-progress target.
#[test]
fn in_progress_opt_in_builds_them() {
    let dir = scratch("in_progress_on");
    put(dir.join("h.nabi"), "fn main() {}\n").unwrap();
    let out = nabic(
        None,
        &dir,
        &[
            "build",
            "h.nabi",
            "--emit=obj",
            "--target",
            "riscv64gc-unknown-linux-gnu",
            "-o",
            "h.o",
        ],
    );
    assert_exit(&out, 0);
    let o = fs::read(dir.join("h.o")).unwrap();
    // ELF, EM_RISCV.
    assert_eq!((&o[..4], [o[18], o[19]]), (&b"\x7fELF"[..], [243, 0]));
}
