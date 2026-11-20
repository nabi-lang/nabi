//! The Android NDK, found as Gradle and the NDK's own scripts find it.
//!
//! A link reads only the NDK's sysroot: the crt objects and the stub
//! `libc.so` / `libm.so` / `libdl.so` of the target's API level, which
//! the device's own libraries replace at run time. The sysroot is the
//! same on every host, so any host's NDK serves.

use crate::TargetSpec;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The lowest API level (Android 10): bionic's loader runs ELF TLS from
/// here on, which the runtime's thread block is.
pub const MIN_API: u32 = 29;

/// Where the NDK installs: `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT`, else
/// the newest `ndk/<version>` of the SDK (`ANDROID_HOME`,
/// `ANDROID_SDK_ROOT`, else Android Studio's default place).
pub fn ndk() -> Result<PathBuf, String> {
    find(&|k| std::env::var_os(k), default_sdk())
}

/// Android Studio's SDK directory on this host.
fn default_sdk() -> Option<PathBuf> {
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA")?;
        return Some(Path::new(&local).join("Android").join("Sdk"));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Android/sdk")
    } else {
        home.join("Android/Sdk")
    })
}

const HINT: &str = "set ANDROID_NDK_HOME to the NDK's directory (Android Studio: SDK Manager, \
                    SDK Tools, NDK (Side by side); or unpack it from \
                    https://developer.android.com/ndk/downloads)";

fn find(env: &dyn Fn(&str) -> Option<OsString>, sdk: Option<PathBuf>) -> Result<PathBuf, String> {
    let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    for k in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
        if let Some(d) = var(k) {
            return if d.join("toolchains").is_dir() {
                Ok(d)
            } else {
                Err(format!(
                    "{k}={} is not an Android NDK (it has no toolchains/): {HINT}",
                    d.display()
                ))
            };
        }
    }
    let sdks: Vec<PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(|k| var(k))
        .chain(sdk)
        .collect();
    for s in &sdks {
        if let Some(d) = newest(&s.join("ndk")) {
            return Ok(d);
        }
    }
    let looked = sdks
        .iter()
        .map(|s| s.join("ndk").display().to_string())
        .collect::<Vec<_>>();
    Err(if looked.is_empty() {
        format!("no Android NDK found: {HINT}")
    } else {
        format!(
            "no Android NDK found (looked in {}): {HINT}",
            looked.join(", ")
        )
    })
}

/// The highest version directory under `dir` (`ndk/30.0.16248370`).
fn newest(dir: &Path) -> Option<PathBuf> {
    let key = |p: &Path| -> Vec<u64> {
        p.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .split('.')
            .map(|x| x.parse().unwrap_or(0))
            .collect()
    };
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("toolchains").is_dir())
        .max_by(|a, b| key(a).cmp(&key(b)))
}

/// The NDK's sysroot: under any host's prebuilt toolchain.
pub fn sysroot(ndk: &Path) -> Result<PathBuf, String> {
    let pre = ndk.join("toolchains/llvm/prebuilt");
    ["darwin-x86_64", "linux-x86_64", "windows-x86_64"]
        .iter()
        .map(|h| pre.join(h).join("sysroot"))
        .find(|s| s.is_dir())
        .ok_or_else(|| {
            format!(
                "the Android NDK at {} has no sysroot under {}: reinstall it, or {HINT}",
                ndk.display(),
                pre.display()
            )
        })
}

/// The sysroot's name of the target's architecture.
#[must_use]
pub fn triple(t: &TargetSpec) -> &'static str {
    match t.arch.as_str() {
        "aarch64" => "aarch64-linux-android",
        _ => "x86_64-linux-android",
    }
}

/// The target's API level (its `deployment_target`).
pub fn api(t: &TargetSpec) -> Result<u32, String> {
    level(&t.deployment_target)
}

/// An API level as `ANDROID_PLATFORM` gives it: `29` or `android-29`.
pub fn level(v: &str) -> Result<u32, String> {
    let n = v.strip_prefix("android-").unwrap_or(v);
    match n.parse::<u32>() {
        Ok(n) if n >= MIN_API => Ok(n),
        Ok(n) => Err(format!(
            "Android API level {n} is below Nabi's lowest, {MIN_API} (Android 10, the first \
             with ELF TLS): write ANDROID_PLATFORM={MIN_API} or higher"
        )),
        Err(_) => Err(format!(
            "`{v}` is not an Android API level: write a number such as ANDROID_PLATFORM={MIN_API}"
        )),
    }
}

/// The directory a link takes the crt objects and C libraries from:
/// `<sysroot>/usr/lib/<triple>/<api>/`.
pub fn lib_dir(t: &TargetSpec) -> Result<PathBuf, String> {
    lib_dir_in(&ndk()?, t)
}

fn lib_dir_in(ndk: &Path, t: &TargetSpec) -> Result<PathBuf, String> {
    let api = api(t)?;
    let base = sysroot(ndk)?.join("usr/lib").join(triple(t));
    let d = base.join(api.to_string());
    if d.join("libc.so").is_file() {
        return Ok(d);
    }
    let have = std::fs::read_dir(&base)
        .map(|r| {
            let mut v: Vec<u32> = r
                .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect();
            v.sort_unstable();
            v
        })
        .unwrap_or_default();
    Err(match (have.first(), have.last()) {
        (Some(lo), Some(hi)) => format!(
            "the Android NDK at {} has no API {api} libraries for {} (it has {lo} to {hi}): \
             write ANDROID_PLATFORM={hi} or lower, or install a newer NDK",
            ndk.display(),
            t.name
        ),
        _ => format!(
            "the Android NDK at {} has no libraries for {} (no {}): install NDK r23 or later",
            ndk.display(),
            t.name,
            base.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nabic-ndk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_ndk(at: &Path, apis: &[u32]) {
        let lib = at.join("toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib");
        for api in apis {
            let d = lib.join("aarch64-linux-android").join(api.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("libc.so"), b"").unwrap();
        }
    }

    #[test]
    fn finds_the_ndk() {
        let d = tmp("find");
        let none = |_: &str| None;
        let e = find(&none, None).unwrap_err();
        assert!(
            e.starts_with("no Android NDK found: set ANDROID_NDK_HOME"),
            "{e}"
        );
        let sdk = d.join("sdk");
        fake_ndk(&sdk.join("ndk/27.2.12479018"), &[29]);
        fake_ndk(&sdk.join("ndk/30.0.16248370"), &[29]);
        assert_eq!(
            find(&none, Some(sdk.clone())).unwrap(),
            sdk.join("ndk/30.0.16248370")
        );
        let home = d.join("own");
        fake_ndk(&home, &[29]);
        let h = home.clone().into_os_string();
        let env = move |k: &str| (k == "ANDROID_NDK_HOME").then(|| h.clone());
        assert_eq!(find(&env, Some(sdk.clone())).unwrap(), home);
        let bad = d.join("nothing").into_os_string();
        let env = move |k: &str| (k == "ANDROID_NDK_ROOT").then(|| bad.clone());
        let e = find(&env, Some(sdk)).unwrap_err();
        assert!(
            e.contains("ANDROID_NDK_ROOT=") && e.contains("is not an Android NDK"),
            "{e}"
        );
        let e = find(&none, Some(d.join("empty"))).unwrap_err();
        assert!(e.contains("looked in ") && e.contains("empty"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn api_levels() {
        assert_eq!(level("29"), Ok(29));
        assert_eq!(level("android-34"), Ok(34));
        let e = level("26").unwrap_err();
        assert!(
            e.contains("below Nabi's lowest, 29") && e.contains("ANDROID_PLATFORM=29"),
            "{e}"
        );
        let e = level("q").unwrap_err();
        assert!(e.contains("`q` is not an Android API level"), "{e}");
    }

    #[test]
    fn library_directory() {
        let d = tmp("lib");
        fake_ndk(&d, &[29, 35]);
        let mut t = TargetSpec::builtin("aarch64-linux-android").unwrap();
        let lib = lib_dir_in(&d, &t).unwrap();
        assert!(
            lib.ends_with("sysroot/usr/lib/aarch64-linux-android/29"),
            "{}",
            lib.display()
        );
        t.deployment_target = "36".to_owned();
        let e = lib_dir_in(&d, &t).unwrap_err();
        assert!(
            e.contains("no API 36") && e.contains("(it has 29 to 35)"),
            "{e}"
        );
        assert!(e.contains("ANDROID_PLATFORM=35 or lower"), "{e}");
        let x = TargetSpec::builtin("x86_64-linux-android").unwrap();
        let e = lib_dir_in(&d, &x).unwrap_err();
        assert!(e.contains("install NDK r23 or later"), "{e}");
        let e = sysroot(&d.join("void")).unwrap_err();
        assert!(e.contains("has no sysroot"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
