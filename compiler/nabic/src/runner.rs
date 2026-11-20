//! What runs a built executable of a target this machine cannot run
//! itself, for `nabi run`, `nabi test` and `nabi bench`.
//!
//! `NABI_RUNNER=<program> [args]` (split on whitespace) runs every
//! executable of a non-host target. Without it: `wasmtime` for
//! `wasm32-wasi*`, `node` with the toolchain's `nabi.mjs` for
//! `wasm32-unknown-unknown`, `qemu-<arch>` for another CPU's Linux on
//! Linux (or nothing, when binfmt runs it), and the executable itself for
//! another CPU of the host's OS (32-bit x86 on `x86_64`, 32-bit ARM on an
//! `aarch64` Linux whose CPU runs `AArch32` code), and the
//! target's `simrun.sh` on a booted iOS simulator (`NABI_SIM_DEVICE`
//! picks another device).

use nabic_interface::TargetSpec;
use nabic_target::{Linker, ObjectFormat};
use std::ffi::OsString;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The environment variable that names a runner.
pub const RUNNER_VAR: &str = "NABI_RUNNER";

/// What `wasm32-wasip2` components may use beyond what wasmtime enables
/// by default.
const COMPONENT_FEATURES: &str =
    "component-model-fixed-length-lists=y,component-model-error-context=y";

/// The command words that run an executable of `t` here, the executable
/// itself after them; empty when it runs directly.
///
/// # Errors
/// No runner is set or found: what to install, or to set.
pub fn runner(t: &TargetSpec) -> Result<Vec<OsString>, String> {
    if t.is_host() {
        return Ok(Vec::new());
    }
    if let Some(r) = std::env::var_os(RUNNER_VAR).filter(|r| !r.is_empty()) {
        let words: Vec<OsString> = r
            .to_string_lossy()
            .split_whitespace()
            .map(OsString::from)
            .collect();
        if !words.is_empty() {
            return Ok(words);
        }
    }
    let host = TargetSpec::host();
    let set = format!("or set {RUNNER_VAR} to a program that runs it");
    if t.object_format == ObjectFormat::Wasm {
        if t.reactor() {
            let node = on_path("node").ok_or_else(|| {
                format!(
                    "cannot run a {} module here: `node` is not on PATH; install Node.js \
                     (https://nodejs.org), {set}",
                    t.name
                )
            })?;
            let host_js = nabic_interface::sysroot()
                .targets
                .join(&t.name)
                .join("nabi.mjs");
            if !host_js.is_file() {
                return Err(format!(
                    "cannot run a {} module here: {} is missing (in the repository, run \
                     targets/build.sh {})",
                    t.name,
                    host_js.display(),
                    t.name
                ));
            }
            return Ok(vec![
                node.into(),
                "--stack-size=8000".into(),
                host_js.into(),
            ]);
        }
        let wasmtime = on_path("wasmtime").ok_or_else(|| {
            format!(
                "cannot run a {} module here: `wasmtime` is not on PATH; install it \
                 (https://wasmtime.dev), {set}",
                t.name
            )
        })?;
        let mut w: Vec<OsString> = vec![wasmtime.into(), "run".into()];
        let words: &[&str] = if t.linker == Linker::WasmComponent {
            &[
                "-S",
                "inherit-env",
                "-S",
                "inherit-network",
                "-S",
                "allow-ip-name-lookup",
                "-W",
                COMPONENT_FEATURES,
            ]
        } else {
            &[
                "-W",
                "threads=y,shared-memory=y",
                "-S",
                "threads=y",
                "-S",
                "inherit-env",
            ]
        };
        w.extend(words.iter().map(OsString::from));
        // The engine stack as deep as a native main thread's, and the
        // host's file system as it is.
        w.extend(
            ["-W", "max-wasm-stack=8388608", "--dir", "/::/"]
                .iter()
                .map(OsString::from),
        );
        return Ok(w);
    }
    if t.os == "android" {
        return Err(format!(
            "cannot run a {} executable on {}: copy it to a device or emulator and run it there \
             (`adb push <program> /data/local/tmp/` then `adb shell /data/local/tmp/<program>`), \
             {set}",
            t.name, host.name
        ));
    }
    if nabic_target::apple::Platform::of(t) == Some(nabic_target::apple::Platform::Ios) {
        return Err(format!(
            "cannot run a {} executable here: an iOS device runs only apps signed for it; \
             build for `--target aarch64-apple-ios-sim` to run on the simulator, or link the \
             library into an Xcode app",
            t.name
        ));
    }
    if nabic_target::apple::Platform::of(t) == Some(nabic_target::apple::Platform::IosSimulator)
        && host.os == "macos"
    {
        return sim_runner(t);
    }
    if t.os != host.os {
        return Err(format!(
            "cannot run a {} executable on {}: {set} (e.g. `wine` for Windows)",
            t.name, host.name
        ));
    }
    if t.os == "linux" && !same_cpu(t, host) {
        let qemu = qemu_name(t);
        if let Some(q) = on_path(&qemu) {
            let mut w: Vec<OsString> = vec![q.into()];
            // A dynamic executable needs the target's loader and libc.
            if std::env::var_os("QEMU_LD_PREFIX").is_none() {
                let sysroot = Path::new("/usr").join(t.c_target.replace("-unknown", ""));
                if sysroot.is_dir() {
                    w.push("-L".into());
                    w.push(sysroot.into());
                }
            }
            return Ok(w);
        }
        if Path::new("/proc/sys/fs/binfmt_misc").join(&qemu).exists() {
            return Ok(Vec::new());
        }
        return Err(format!(
            "cannot run a {} executable here: `{qemu}` is not on PATH; install qemu-user \
             (Debian, Ubuntu: `apt install qemu-user`), {set}",
            t.name
        ));
    }
    // Another CPU of the host's OS: the OS runs it or says why not.
    Ok(Vec::new())
}

/// The command that runs `exe` through `runner` (from [`runner`]).
#[must_use]
pub fn run_command(runner: &[OsString], exe: &Path) -> Command {
    match runner.split_first() {
        Some((program, args)) => {
            let mut c = Command::new(program);
            c.args(args).arg(exe);
            c
        }
        None => Command::new(exe),
    }
}

/// Whether the host's CPU runs `t`'s code (32-bit x86 on `x86_64` does,
/// 32-bit ARM on an `aarch64` Linux whose CPU has `AArch32` at EL0).
fn same_cpu(t: &TargetSpec, host: &TargetSpec) -> bool {
    t.arch == host.arch && t.endian == host.endian
        || t.arch == "x86" && host.arch == "x86_64"
        || t.arch == "arm" && host.arch == "aarch64" && host.os == "linux" && aarch32()
}

/// A static ARM ELF executable that exits 0: `mov r0, #0; mov r7, #1`
/// (exit); `svc #0`, loaded at 0x10000.
const AARCH32_EXIT: [u8; 96] = [
    0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, // ident
    2, 0, 40, 0, 1, 0, 0, 0, // ET_EXEC, EM_ARM, version
    0x54, 0, 1, 0, 52, 0, 0, 0, 0, 0, 0, 0, // entry, phoff, shoff
    0, 4, 0, 5, 52, 0, 32, 0, 1, 0, 0, 0, 0, 0, 0, 0, // flags, sizes
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, // PT_LOAD
    96, 0, 0, 0, 96, 0, 0, 0, 5, 0, 0, 0, 0, 0x10, 0, 0, // sizes, R+X, align
    0x00, 0x00, 0xa0, 0xe3, 0x01, 0x70, 0xa0, 0xe3, 0x00, 0x00, 0x00, 0xef, // code
];

/// Whether this machine runs 32-bit ARM executables itself: the kernel
/// has `CONFIG_COMPAT` and the CPU `AArch32` at EL0 (Neoverse N1, Cortex-A72;
/// not Neoverse N2 or Apple silicon). No register says so to user
/// space, so it runs [`AARCH32_EXIT`] once.
fn aarch32() -> bool {
    static RUNS: OnceLock<bool> = OnceLock::new();
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        return false;
    }
    *RUNS.get_or_init(|| {
        let exe = std::env::temp_dir().join(format!("nabi-aarch32-{}", std::process::id()));
        let runs = write_exe(&exe, &AARCH32_EXIT).is_ok() && exits_0(&exe);
        let _ = std::fs::remove_file(&exe);
        runs
    })
}

/// `exe` runs and exits 0. ETXTBSY (another thread's fork still holds
/// the file open for writing) is tried again.
fn exits_0(exe: &Path) -> bool {
    for _ in 0..20 {
        let mut c = Command::new(exe);
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match c.status() {
            Ok(s) => return s.success(),
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
    false
}

/// `bytes` as an executable file at `path`.
#[cfg(unix)]
fn write_exe(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_exe(_: &Path, _: &[u8]) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// qemu-user's program for `t`'s CPU.
fn qemu_name(t: &TargetSpec) -> String {
    let cpu = match (t.arch.as_str(), t.endian.as_str()) {
        ("x86", _) => "i386",
        ("powerpc64", "little") => "ppc64le",
        (a, _) => a,
    };
    format!("qemu-{cpu}")
}

/// The environment variable naming the simulator device to run on.
pub const SIM_DEVICE_VAR: &str = "NABI_SIM_DEVICE";

/// An iOS simulator executable runs on a simulator device through the
/// target's `simrun.sh` (`xcrun simctl spawn`, the environment passed on).
fn sim_runner(t: &TargetSpec) -> Result<Vec<OsString>, String> {
    let script = nabic_interface::sysroot()
        .targets
        .join(&t.name)
        .join("simrun.sh");
    if !script.is_file() {
        return Err(format!(
            "cannot run a {} executable here: {} is missing (in the repository, run \
             targets/build.sh {})",
            t.name,
            script.display(),
            t.name
        ));
    }
    let device = std::env::var_os(SIM_DEVICE_VAR)
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "booted".into());
    Ok(vec!["/bin/sh".into(), script.into(), device])
}

/// `name` in a directory of PATH.
fn on_path(name: &str) -> Option<PathBuf> {
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::{AARCH32_EXIT, TargetSpec, aarch32, qemu_name, runner, same_cpu};

    #[test]
    fn host_runs_itself() {
        assert!(runner(TargetSpec::host()).unwrap().is_empty());
    }

    /// The simulator runs through the target's `simrun.sh`, on the
    /// booted device; without the script the error says how to make it.
    #[test]
    #[cfg(target_os = "macos")]
    fn ios_simulator_runs_through_simrun() {
        match runner(&TargetSpec::builtin("aarch64-apple-ios-sim").unwrap()) {
            Ok(w) => {
                assert_eq!(w[0], "/bin/sh");
                assert!(
                    w[1].to_string_lossy()
                        .ends_with("aarch64-apple-ios-sim/simrun.sh")
                );
            }
            Err(e) => assert!(
                e.contains("simrun.sh is missing") && e.contains("targets/build.sh"),
                "{e}"
            ),
        }
    }

    #[test]
    fn ios_device_says_where_to_run() {
        let e = runner(&TargetSpec::builtin("aarch64-apple-ios").unwrap()).unwrap_err();
        assert!(
            e.contains("an iOS device runs only apps signed for it")
                && e.contains("--target aarch64-apple-ios-sim"),
            "{e}"
        );
    }

    #[test]
    fn qemu_names() {
        let names: Vec<String> = [
            "aarch64-unknown-linux-gnu",
            "armv7-unknown-linux-gnueabihf",
            "powerpc64le-unknown-linux-gnu",
            "i686-unknown-linux-gnu",
            "s390x-unknown-linux-gnu",
        ]
        .iter()
        .map(|t| qemu_name(&TargetSpec::builtin(t).unwrap()))
        .collect();
        assert_eq!(
            names,
            [
                "qemu-aarch64",
                "qemu-arm",
                "qemu-ppc64le",
                "qemu-i386",
                "qemu-s390x"
            ]
        );
        let x64 = TargetSpec::builtin("x86_64-unknown-linux-gnu").unwrap();
        let x86 = TargetSpec::builtin("i686-unknown-linux-gnu").unwrap();
        assert!(same_cpu(&x86, &x64) && !same_cpu(&x64, &x86));
    }

    /// The probe is an ARM EABI5 hard-float executable whose entry is its
    /// code, in its one segment.
    #[test]
    fn aarch32_probe_is_an_arm_executable() {
        let e = &AARCH32_EXIT;
        let u16_at = |i: usize| u16::from_le_bytes([e[i], e[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([e[i], e[i + 1], e[i + 2], e[i + 3]]);
        assert_eq!(&e[..6], b"\x7fELF\x01\x01");
        assert_eq!((u16_at(16), u16_at(18)), (2, 40));
        assert_eq!(u32_at(36), 0x0500_0400);
        assert_eq!((u32_at(28), u16_at(42), u16_at(44)), (52, 32, 1));
        let (vaddr, filesz) = (u32_at(52 + 8), u32_at(52 + 16));
        assert_eq!((u32_at(52), filesz as usize), (1, e.len()));
        assert_eq!(u32_at(24), vaddr + 84);
    }

    /// 32-bit ARM runs directly exactly where the probe runs, and only
    /// on an `aarch64` Linux.
    #[test]
    fn armv7_runs_directly_where_aarch32_runs() {
        let arm = TargetSpec::builtin("armv7-unknown-linux-gnueabihf").unwrap();
        let host = TargetSpec::host();
        let arm64_linux = host.arch == "aarch64" && host.os == "linux";
        assert_eq!(same_cpu(&arm, host), arm64_linux && aarch32());
        if !arm64_linux {
            assert!(!aarch32());
        }
        if same_cpu(&arm, host) && std::env::var_os(super::RUNNER_VAR).is_none() {
            assert!(runner(&arm).unwrap().is_empty());
        }
    }
}
