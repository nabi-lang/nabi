//! Where a target's C library headers and libraries come from.
//!
//! - Apple: the SDK (`SDKROOT`, else Xcode's or the command line tools').
//! - Linux, wasm32: the toolchain's sysroot for the target
//!   (`lib/nabi/targets/<target>/sysroot`, made by `targets/build.sh` from
//!   the same pinned glibc, musl and wasi-libc it links with); the host's
//!   own `/usr/include` when the target is the host and none is shipped.
//! - Windows: the Windows SDK's UCRT headers and Visual Studio's
//!   (`INCLUDE`, else `vswhere`), on a Windows host.

use nabic_target::TargetSpec;
use std::path::PathBuf;

/// The toolchain's directory for target `t`
/// (`lib/nabi/targets/<target>`).
#[must_use]
pub fn target_dir(t: &TargetSpec) -> PathBuf {
    nabic_interface::sysroot().targets.join(&t.name)
}

/// `(host, sdk, sysroot)` for `sysinc::dirs`.
pub fn system(t: &TargetSpec) -> Result<(bool, Option<PathBuf>, Option<PathBuf>), String> {
    // No C library: the compiler's own freestanding headers.
    if t.libc == nabic_target::Libc::None {
        return Ok((false, None, None));
    }
    if t.vendor == "apple" {
        return Ok((false, Some(nabic_target::apple::sdk_for(t)?), None));
    }
    // Android: the NDK's sysroot.
    if t.libc == nabic_target::Libc::Bionic {
        let ndk = nabic_target::android::ndk()?;
        return Ok((false, None, Some(nabic_target::android::sysroot(&ndk)?)));
    }
    let shipped = target_dir(t).join("sysroot");
    if shipped.is_dir() {
        return Ok((false, None, Some(shipped)));
    }
    if t.is_host() {
        return Ok((true, None, None));
    }
    if t.os == "windows" && TargetSpec::host().os == "windows" {
        return Ok((true, None, None));
    }
    Err(format!(
        "no C headers for {}: {} is missing (this toolchain does not ship them; in the \
         repository, run targets/build.sh {}), or pass `--sysroot <dir>` with the target's \
         headers",
        t.name,
        shipped.display(),
        t.name
    ))
}
