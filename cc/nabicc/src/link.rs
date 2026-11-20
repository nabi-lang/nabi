//! Linking with Nabi's own linker against the target's C library.

use crate::headers;
use nabic_target::{ObjectFormat, TargetSpec};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Link `inputs` (objects, archives, `-l`, `-L`, `-Wl,` options) into
/// executable `out` for `t`; `tmp` holds what the link makes on the way.
pub fn executable(
    t: &TargetSpec,
    inputs: &[OsString],
    out: &Path,
    tmp: &Path,
    debug: bool,
) -> Result<(), String> {
    let mut args: Vec<OsString> = Vec::new();
    for a in inputs {
        match a.to_str().and_then(|s| s.strip_prefix("-Wl,")) {
            Some(rest) => args.extend(rest.split(',').map(OsString::from)),
            None => args.push(a.clone()),
        }
    }
    // wasi-libc's libc.a holds the math library too.
    if t.object_format == ObjectFormat::Wasm {
        args.retain(|a| a.to_str() != Some("-lm"));
    }
    // Options the linker has no use for.
    args.retain(|a| {
        !matches!(
            a.to_str(),
            Some(
                "-lpthread"
                    | "-ldl"
                    | "-lrt"
                    | "-lc"
                    | "-nostdlib"
                    | "-nodefaultlibs"
                    | "-nostartfiles"
            )
        )
    });
    let libc: Option<PathBuf> = match t.object_format {
        ObjectFormat::MachO => None,
        ObjectFormat::Coff => {
            // C's `main` under the startup's `nabi_main`.
            let shim = tmp.join("nabicc_main.obj");
            std::fs::write(&shim, crate::main_shim(t)?)
                .map_err(|e| format!("{}: {e}", shim.display()))?;
            args.push(shim.into_os_string());
            args.extend(nabic_driver::windows_link_args(
                t,
                &[],
                nabic_driver::Subsystem::Console,
                debug,
                tmp,
            )?);
            None
        }
        // Android: the NDK's crt objects and bionic's stubs.
        _ if t.libc == nabic_target::Libc::Bionic => Some(nabic_target::android::lib_dir(t)?),
        _ => Some(headers::target_dir(t)),
    };
    if t.object_format == ObjectFormat::Elf {
        // Programs that call libm functions name it; the toolchain's libc
        // keeps it apart.
        args.push("-lm".into());
    }
    // 32-bit ARM divides through `__aeabi_*` helpers, wasi-libc multiplies
    // through `__multi3`: the runtime archive's compiler builtins have
    // them.
    if t.arch == "arm" || t.object_format == ObjectFormat::Wasm {
        let rt = headers::target_dir(t).join(t.runtime_lib());
        if rt.is_file() {
            args.push(rt.into_os_string());
        }
    }
    nabild::link_executable(t, &args, out, false, libc.as_deref())?;
    // Mach-O keeps DWARF in the objects: gather it while they exist.
    if debug && t.object_format == ObjectFormat::MachO {
        let _ = nabild::macho::dsym::write(out)?;
    }
    Ok(())
}
