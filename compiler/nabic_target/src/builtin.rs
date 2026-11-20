//! The built-in targets, the ones of them still in progress, and the
//! names of the targets not supported.

use crate::{CAbi, Libc, Linker, ObjectFormat, StackProbes, TargetSpec};

/// Every built-in target's name, the supported and the in-progress ones.
pub const BUILTIN: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "wasm32-unknown-unknown",
    "wasm32-wasip2",
    "wasm32-wasip1-threads",
    "i686-unknown-linux-gnu",
    "i686-pc-windows-msvc",
    "armv7-unknown-linux-gnueabihf",
    "armv7-unknown-linux-musleabihf",
    "riscv64gc-unknown-linux-gnu",
    "powerpc64le-unknown-linux-gnu",
    "x86_64-unknown-freebsd",
    "riscv32imac-unknown-none-elf",
    "s390x-unknown-linux-gnu",
    "aarch64-linux-android",
    "x86_64-linux-android",
    "aarch64-apple-ios",
    "aarch64-apple-ios-sim",
];

/// The environment variable that lets `--target` take an in-progress
/// target: `1` turns it on. A maintainer's testing switch.
pub const IN_PROGRESS_VAR: &str = "NABI_IN_PROGRESS_TARGETS";

/// Built-in targets not yet tested on real hardware: the code is in, but
/// `--target` refuses them unless [`IN_PROGRESS_VAR`] is `1`. The name,
/// what it is, and what to build now.
const IN_PROGRESS: &[(&str, &str, &str)] = &[
    (
        "aarch64-apple-ios",
        "iOS devices",
        "for now build for the simulator with `--target aarch64-apple-ios-sim`",
    ),
    (
        "aarch64-linux-android",
        "64-bit ARM Android devices",
        "for now build for the emulator with `--target x86_64-linux-android`",
    ),
    (
        "riscv64gc-unknown-linux-gnu",
        "64-bit RISC-V Linux",
        "no supported target stands in for it yet",
    ),
    (
        "powerpc64le-unknown-linux-gnu",
        "64-bit POWER Linux",
        "no supported target stands in for it yet",
    ),
    (
        "s390x-unknown-linux-gnu",
        "IBM Z Linux",
        "no supported target stands in for it yet",
    ),
    (
        "riscv32imac-unknown-none-elf",
        "32-bit RISC-V bare metal",
        "no supported target stands in for it yet",
    ),
];

/// `name` is a built-in target still in progress.
#[must_use]
pub fn in_progress(name: &str) -> bool {
    IN_PROGRESS.iter().any(|(n, ..)| *n == name)
}

/// [`IN_PROGRESS_VAR`] is `1`: `--target` takes the in-progress targets.
#[must_use]
pub fn in_progress_on() -> bool {
    std::env::var_os(IN_PROGRESS_VAR).is_some_and(|v| v == "1")
}

/// The built-in targets `--target` takes: the supported ones, and the
/// in-progress ones with [`IN_PROGRESS_VAR`] on.
pub fn available() -> impl Iterator<Item = &'static str> {
    let all = in_progress_on();
    BUILTIN
        .iter()
        .copied()
        .filter(move |t| all || !in_progress(t))
}

/// Why in-progress target `name` is refused, and what to build now.
fn refusal(name: &str) -> Option<String> {
    let &(_, what, now) = IN_PROGRESS.iter().find(|(n, ..)| *n == name)?;
    Some(format!(
        "target `{name}` ({what}) is in progress: it is not tested on real hardware yet \
         and comes in a later release; {now}"
    ))
}

/// Targets people look for that are not supported: the name, what it
/// is, and the target to use instead.
const NOT_SUPPORTED: &[(&str, &str, Option<&str>)] = &[
    ("arm-unknown-linux-gnueabi", "ARMv6, soft float", None),
    (
        "armv7-linux-androideabi",
        "32-bit Android",
        Some("aarch64-linux-android"),
    ),
    ("thumbv6m-none-eabi", "Cortex-M", None),
    ("thumbv7m-none-eabi", "Cortex-M", None),
    ("thumbv7em-none-eabi", "Cortex-M", None),
    ("thumbv7em-none-eabihf", "Cortex-M", None),
    ("thumbv8m.main-none-eabi", "Cortex-M", None),
    ("thumbv8m.main-none-eabihf", "Cortex-M", None),
    (
        "x86_64-apple-ios",
        "the iOS simulator on Intel Macs",
        Some("aarch64-apple-ios-sim"),
    ),
    (
        "x86_64-apple-darwin",
        "Intel Macs",
        Some("aarch64-apple-darwin"),
    ),
];

/// Why `name`, an in-progress target (without [`IN_PROGRESS_VAR`]) or
/// one not supported, is refused, and what to use instead.
pub(crate) fn unavailable(name: &str) -> Option<String> {
    if !in_progress_on() {
        if let Some(why) = refusal(name) {
            return Some(why);
        }
    }
    let &(_, what, alt) = NOT_SUPPORTED.iter().find(|(n, ..)| *n == name)?;
    let instead = match alt {
        Some(a) if in_progress(a) && !in_progress_on() => {
            let now = IN_PROGRESS
                .iter()
                .find(|(n, ..)| *n == a)
                .map_or("", |e| e.2);
            format!("; `{a}` is in progress, {now}")
        }
        Some(a) => format!("; use `{a}`"),
        None => String::new(),
    };
    Some(format!(
        "target `{name}` is not supported ({what}){instead}"
    ))
}

/// nabi's thread stack: 8 MiB, as Linux's default rlimit and macOS give
/// the main thread (nested `await` runs on it).
const STACK: u64 = 8 << 20;

fn s(v: &str) -> String {
    v.to_owned()
}

fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| (*x).to_owned()).collect()
}

/// A Linux ELF target on glibc, `name` as its C triple.
fn linux_gnu(name: &str, arch: &str, emulation: &str, loader: &str) -> TargetSpec {
    TargetSpec {
        name: s(name),
        c_target: s(name),
        deployment_target: s(""),
        arch: s(arch),
        os: s("linux"),
        env: s("gnu"),
        abi: s(""),
        vendor: s("unknown"),
        family: s("unix"),
        endian: s("little"),
        pointer_width: 64,
        wide_align: 8,
        max_atomic_width: 64,
        c_abi: CAbi::Other,
        cpu: s("generic"),
        features: s(""),
        object_format: ObjectFormat::Elf,
        linker: Linker::Elf,
        pre_link_args: v(&["-m", emulation]),
        crt_objects: v(&["Scrt1.o", "crti.o"]),
        // core's float methods call libm; glibc keeps it out of libc.
        late_link_args: v(&[
            "--as-needed",
            "-lm",
            "--no-as-needed",
            "-lc",
            "libc_nonshared.a",
            "crtn.o",
        ]),
        dynamic_linking: true,
        dynamic_loader: s(loader),
        libc: Libc::Glibc,
        rust_target: s(name),
        exe_suffix: s(""),
        staticlib_prefix: s("lib"),
        staticlib_suffix: s(".a"),
        dylib_prefix: s("lib"),
        dylib_suffix: s(".so"),
        c_symbol_prefix: s(""),
        stack_size: STACK,
        stack_probes: StackProbes::None,
        threads: true,
        native_backend: false,
        repl: false,
    }
}

/// The musl variant `name` of glibc target `t`: static executables.
fn musl(name: &str, t: TargetSpec) -> TargetSpec {
    TargetSpec {
        name: s(name),
        c_target: s(name),
        rust_target: s(name),
        env: s("musl"),
        crt_objects: v(&["crt1.o", "crti.o"]),
        late_link_args: v(&["libc.a", "crtn.o"]),
        dynamic_linking: false,
        dynamic_loader: s(""),
        libc: Libc::Musl,
        // Static: a session host has no dlsym to bind its cells.
        repl: false,
        ..t
    }
}

/// `x86_64` / `aarch64` Linux: nabi's own backend, the REPL.
fn native(t: TargetSpec) -> TargetSpec {
    TargetSpec {
        native_backend: true,
        repl: true,
        stack_probes: StackProbes::Inline,
        ..t
    }
}

fn x86_64(t: TargetSpec) -> TargetSpec {
    native(TargetSpec {
        c_abi: CAbi::X86_64SysV,
        cpu: s("x86-64"),
        ..t
    })
}

fn aarch64(t: TargetSpec) -> TargetSpec {
    native(TargetSpec {
        c_abi: CAbi::Aarch64,
        features: s("+neon"),
        ..t
    })
}

/// i686: SSE2, nabi's own backend.
fn i686(t: TargetSpec) -> TargetSpec {
    TargetSpec {
        arch: s("x86"),
        pointer_width: 32,
        cpu: s("pentium4"),
        stack_probes: StackProbes::Inline,
        native_backend: true,
        ..t
    }
}

/// arm64 iOS, `abi` `sim` for the simulator: an app's code (no REPL, no
/// JIT), linked against the iOS SDK.
fn ios(name: &str, abi: &str, cpu: &str) -> TargetSpec {
    let t = aarch64(apple(name));
    TargetSpec {
        c_target: s("arm64-apple-ios"),
        deployment_target: s(crate::apple::IOS_MIN),
        os: s("ios"),
        abi: s(abi),
        cpu: s(cpu),
        features: s(""),
        repl: false,
        ..t
    }
}

/// arm64 macOS.
fn apple(name: &str) -> TargetSpec {
    TargetSpec {
        c_target: s("arm64-apple-macosx"),
        // What the runtime library is built for.
        deployment_target: s("11.0"),
        os: s("macos"),
        env: s(""),
        vendor: s("apple"),
        object_format: ObjectFormat::MachO,
        linker: Linker::MachO,
        pre_link_args: v(&["-arch", "arm64"]),
        crt_objects: Vec::new(),
        late_link_args: v(&["-lSystem"]),
        dynamic_loader: s(""),
        libc: Libc::LibSystem,
        dylib_suffix: s(".dylib"),
        c_symbol_prefix: s("_"),
        ..linux_gnu(name, "aarch64", "", "")
    }
}

fn windows(name: &str, arch: &str, machine: &str) -> TargetSpec {
    TargetSpec {
        os: s("windows"),
        env: s("msvc"),
        vendor: s("pc"),
        family: s("windows"),
        object_format: ObjectFormat::Coff,
        linker: Linker::Coff,
        pre_link_args: vec![format!("/machine:{machine}")],
        crt_objects: Vec::new(),
        late_link_args: Vec::new(),
        dynamic_loader: s(""),
        libc: Libc::Ucrt,
        exe_suffix: s(".exe"),
        staticlib_prefix: s(""),
        staticlib_suffix: s(".lib"),
        dylib_prefix: s(""),
        dylib_suffix: s(".dll"),
        ..linux_gnu(name, arch, "", "")
    }
}

/// wasm32 against wasi-libc: a command module.
fn wasm32(name: &str, rust_target: &str) -> TargetSpec {
    TargetSpec {
        os: s("wasi"),
        env: s(""),
        family: s("wasm"),
        pointer_width: 32,
        c_abi: CAbi::Wasm32,
        // SIMD128.
        features: s("+simd128"),
        object_format: ObjectFormat::Wasm,
        linker: Linker::Wasm,
        pre_link_args: Vec::new(),
        crt_objects: v(&["crt1-command.o"]),
        late_link_args: v(&["libc.a"]),
        dynamic_linking: false,
        dynamic_loader: s(""),
        libc: Libc::WasiLibc,
        rust_target: s(rust_target),
        exe_suffix: s(".wasm"),
        threads: false,
        native_backend: true,
        ..linux_gnu(name, "wasm32", "", "")
    }
}

/// Android on bionic, API level `android::MIN_API` and up: a PIE that
/// bionic's loader runs, the NDK's crt objects and stub libraries
/// (`android::lib_dir`).
fn android(name: &str, arch: &str, emulation: &str, loader: &str) -> TargetSpec {
    TargetSpec {
        deployment_target: crate::android::MIN_API.to_string(),
        os: s("android"),
        env: s(""),
        crt_objects: v(&["crtbegin_dynamic.o"]),
        late_link_args: v(&[
            "--as-needed",
            "-lm",
            "-ldl",
            "--no-as-needed",
            "-lc",
            "crtend_android.o",
        ]),
        libc: Libc::Bionic,
        ..linux_gnu(name, arch, emulation, loader)
    }
}

/// The built-in target `name`.
pub fn spec(name: &str) -> Option<TargetSpec> {
    Some(match name {
        "x86_64-unknown-linux-gnu" => x86_64(linux_gnu(
            name,
            "x86_64",
            "elf_x86_64",
            "/lib64/ld-linux-x86-64.so.2",
        )),
        "aarch64-unknown-linux-gnu" => aarch64(linux_gnu(
            name,
            "aarch64",
            "aarch64linux",
            "/lib/ld-linux-aarch64.so.1",
        )),
        "x86_64-unknown-linux-musl" => musl(name, spec("x86_64-unknown-linux-gnu")?),
        "aarch64-unknown-linux-musl" => musl(name, spec("aarch64-unknown-linux-gnu")?),
        "armv7-unknown-linux-musleabihf" => musl(name, spec("armv7-unknown-linux-gnueabihf")?),
        "i686-unknown-linux-gnu" => i686(TargetSpec {
            wide_align: 4,
            c_abi: CAbi::X86SysV,
            ..linux_gnu(name, "x86", "elf_i386", "/lib/ld-linux.so.2")
        }),
        "armv7-unknown-linux-gnueabihf" => TargetSpec {
            pointer_width: 32,
            c_abi: CAbi::ArmHf,
            // NEON; the thread pointer from TPIDRURO, not
            // glibc's `__aeabi_read_tp` (in ld.so).
            features: s("+vfp3,+d32,+neon,+read-tp-tpidruro"),
            stack_probes: StackProbes::Inline,
            native_backend: true,
            ..linux_gnu(name, "arm", "armelf_linux_eabi", "/lib/ld-linux-armhf.so.3")
        },
        "aarch64-apple-darwin" => {
            let t = aarch64(apple(name));
            TargetSpec {
                cpu: s("apple-m1"),
                features: s(""),
                ..t
            }
        }
        // A9 and later devices; the simulator runs on Apple silicon.
        "aarch64-apple-ios" => ios(name, "", "apple-a7"),
        "aarch64-apple-ios-sim" => ios(name, "sim", "apple-m1"),
        "x86_64-pc-windows-msvc" => TargetSpec {
            c_abi: CAbi::X86_64Win,
            cpu: s("x86-64"),
            stack_probes: StackProbes::Call,
            native_backend: true,
            repl: true,
            ..windows(name, "x86_64", "x64")
        },
        "aarch64-pc-windows-msvc" => TargetSpec {
            c_abi: CAbi::Aarch64,
            features: s("+neon"),
            stack_probes: StackProbes::Call,
            native_backend: true,
            repl: true,
            ..windows(name, "aarch64", "arm64")
        },
        "i686-pc-windows-msvc" => {
            let mut t = i686(windows(name, "x86", "x86"));
            // 4 GiB address space on 64-bit Windows (the heap),
            // no SafeSEH: nabi raises no SEH and the runtime's handler is
            // vectored.
            t.pre_link_args
                .extend(v(&["/largeaddressaware", "/safeseh:no"]));
            TargetSpec {
                c_abi: CAbi::X86Win,
                c_symbol_prefix: s("_"),
                stack_probes: StackProbes::Call,
                ..t
            }
        }
        // A WASI preview 2 component.
        "wasm32-wasip2" => TargetSpec {
            // clang sees plain wasi; the linker makes the component.
            c_target: s("wasm32-wasi"),
            linker: Linker::WasmComponent,
            // The adapter allocates through the module's `cabi_realloc`,
            // which only libc's wasip2.o defines: load it even when
            // nothing else needs that member.
            pre_link_args: v(&["--export-if-defined=cabi_realloc"]),
            ..wasm32(name, "wasm32-wasip2")
        },
        // Preview 1 with wasi-threads: host-provided shared memory and
        // `wasi_thread_start` as each thread's entry.
        "wasm32-wasip1-threads" => TargetSpec {
            c_target: s("wasm32-wasi-threads"),
            features: s("+simd128,+atomics,+bulk-memory,+mutable-globals"),
            pre_link_args: v(&[
                "--shared-memory",
                "--import-memory",
                "--export-memory",
                "--export=wasi_thread_start",
            ]),
            threads: true,
            ..wasm32(name, "wasm32-wasip1-threads")
        },
        // The browser's: a reactor. The host calls `_initialize`, then
        // exports, flushing stdout (nabi's and C's buffers) after each, since
        // no `exit` does.
        "wasm32-unknown-unknown" => TargetSpec {
            os: s("unknown"),
            pre_link_args: v(&[
                "--no-entry",
                "--export-if-defined=nabi_stdio_flush",
                "--export-if-defined=fflush",
            ]),
            crt_objects: v(&["crt1-reactor.o"]),
            // No file system to write a profile to.
            ..wasm32(name, "wasm32-wasip1")
        },
        "riscv64gc-unknown-linux-gnu" => TargetSpec {
            c_target: s("riscv64-unknown-linux-gnu"),
            c_abi: CAbi::Riscv64,
            cpu: s("generic-rv64"),
            features: s("+m,+a,+f,+d,+c"),
            stack_probes: StackProbes::Inline,
            native_backend: true,
            ..linux_gnu(
                name,
                "riscv64",
                "elf64lriscv",
                "/lib/ld-linux-riscv64-lp64d.so.1",
            )
        },
        "powerpc64le-unknown-linux-gnu" => TargetSpec {
            c_abi: CAbi::Ppc64,
            // POWER8, rustc's baseline: VSX, but nothing here needs it.
            cpu: s("ppc64le"),
            stack_probes: StackProbes::Inline,
            native_backend: true,
            ..linux_gnu(name, "powerpc64", "elf64lppc", "/lib64/ld64.so.2")
        },
        "s390x-unknown-linux-gnu" => TargetSpec {
            endian: s("big"),
            c_abi: CAbi::S390x,
            cpu: s("z10"),
            stack_probes: StackProbes::Inline,
            native_backend: true,
            ..linux_gnu(name, "s390x", "elf64_s390", "/lib/ld64.so.1")
        },
        // A PIE against FreeBSD 14's libc (`targets/build.sh`); threads
        // are libthr's.
        "x86_64-unknown-freebsd" => TargetSpec {
            os: s("freebsd"),
            env: s(""),
            crt_objects: v(&["Scrt1.o", "crti.o"]),
            late_link_args: v(&[
                "--as-needed",
                "-lm",
                "--no-as-needed",
                "-lthr",
                "-lc",
                "libc_nonshared.a",
                "crtn.o",
            ]),
            dynamic_loader: s("/libexec/ld-elf.so.1"),
            libc: Libc::FreeBsd,
            repl: false,
            ..x86_64(TargetSpec {
                pre_link_args: v(&["-m", "elf_x86_64_fbsd"]),
                ..linux_gnu(name, "x86_64", "", "")
            })
        },
        "riscv32imac-unknown-none-elf" => TargetSpec {
            c_target: s("riscv32"),
            os: s("none"),
            env: s(""),
            family: s(""),
            pointer_width: 32,
            max_atomic_width: 32,
            c_abi: CAbi::Riscv32,
            cpu: s("generic-rv32"),
            features: s("+m,+a,+c"),
            crt_objects: Vec::new(),
            late_link_args: Vec::new(),
            dynamic_linking: false,
            dynamic_loader: s(""),
            libc: Libc::None,
            rust_target: s("riscv32imac-unknown-none-elf"),
            threads: false,
            native_backend: true,
            ..linux_gnu(name, "riscv32", "elf32lriscv", "")
        },
        // No session host: the REPL needs a host shell.
        "aarch64-linux-android" => TargetSpec {
            repl: false,
            ..aarch64(android(
                name,
                "aarch64",
                "aarch64linux",
                "/system/bin/linker64",
            ))
        },
        // The x86_64 Android ABI: SSE4.2 and POPCNT.
        // 16 KiB pages, as arm64 (64 KiB there already).
        "x86_64-linux-android" => TargetSpec {
            cpu: s("x86-64-v2"),
            pre_link_args: v(&["-m", "elf_x86_64", "-z", "max-page-size=16384"]),
            repl: false,
            ..x86_64(android(
                name,
                "x86_64",
                "elf_x86_64",
                "/system/bin/linker64",
            ))
        },
        _ => return None,
    })
}
