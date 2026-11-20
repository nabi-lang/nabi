//! The Apple SDKs, found as `xcrun` would find them.

use crate::TargetSpec;
use std::path::{Path, PathBuf};

/// An Apple platform: what `LC_BUILD_VERSION` and ld64's
/// `-platform_version` name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    MacOs,
    Ios,
    IosSimulator,
}

impl Platform {
    /// The platform of a Mach-O target, `None` elsewhere.
    #[must_use]
    pub fn of(t: &TargetSpec) -> Option<Self> {
        if t.object_format != crate::ObjectFormat::MachO {
            return None;
        }
        match (t.os.as_str(), t.abi.as_str()) {
            ("macos", _) => Some(Self::MacOs),
            ("ios", "sim") => Some(Self::IosSimulator),
            ("ios", _) => Some(Self::Ios),
            _ => None,
        }
    }

    /// `PLATFORM_*` of `LC_BUILD_VERSION`.
    #[must_use]
    pub const fn id(self) -> u32 {
        match self {
            Self::MacOs => 1,
            Self::Ios => 2,
            Self::IosSimulator => 7,
        }
    }

    /// ld64's `-platform_version` name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Ios => "ios",
            Self::IosSimulator => "ios-simulator",
        }
    }

    /// The environment variable over the target's `deployment_target`.
    #[must_use]
    pub const fn deployment_var(self) -> &'static str {
        match self {
            Self::MacOs => "MACOSX_DEPLOYMENT_TARGET",
            Self::Ios | Self::IosSimulator => "IPHONEOS_DEPLOYMENT_TARGET",
        }
    }

    /// The SDK's directory name, as in `MacOSX.sdk`.
    const fn sdk_name(self) -> &'static str {
        match self {
            Self::MacOs => "MacOSX",
            Self::Ios => "iPhoneOS",
            Self::IosSimulator => "iPhoneSimulator",
        }
    }

    /// `.tbd` `targets:` platform names that serve this platform.
    #[must_use]
    pub const fn tbd_names(self) -> &'static [&'static str] {
        match self {
            Self::MacOs => &["macos", "macosx"],
            Self::Ios => &["ios"],
            Self::IosSimulator => &["ios-simulator"],
        }
    }
}

/// The developer directory: `DEVELOPER_DIR`, else `xcode-select`'s, else
/// the command line tools.
fn developer_dir() -> PathBuf {
    std::env::var_os("DEVELOPER_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::fs::read_link("/var/db/xcode_select_link").ok())
        .unwrap_or_else(|| "/Library/Developer/CommandLineTools".into())
}

/// The macOS SDK as `xcrun` would find it, without running it: `SDKROOT`,
/// else `DEVELOPER_DIR`'s, else `xcode-select`'s, else the CLT.
pub fn sdk() -> Result<PathBuf, String> {
    sdk_of(Platform::MacOs)
}

/// The SDK of Apple target `t`: macOS's, or the iOS device's or
/// simulator's ([`sdk_of`]).
pub fn sdk_for(t: &TargetSpec) -> Result<PathBuf, String> {
    sdk_of(Platform::of(t).unwrap_or(Platform::MacOs))
}

/// The SDK of `p` as `xcrun --sdk` would find it: `SDKROOT`, else the
/// developer directory's. Only Xcode has the iOS SDKs; the command line
/// tools have macOS's alone.
///
/// An `SDKROOT` of another platform's SDK (a macOS one for an iOS
/// target) is ignored, with a note, as rustc does.
pub fn sdk_of(p: Platform) -> Result<PathBuf, String> {
    static NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let root = std::env::var_os("SDKROOT").filter(|s| !s.is_empty());
    find_sdk(p, root.map(PathBuf::from), &developer_dir(), &mut |m| {
        if !NOTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            nabic_core::term::note(m);
        }
    })
}

/// [`sdk_of`] with `SDKROOT` and the developer directory given; `note`
/// says why `root` is passed over.
fn find_sdk(
    p: Platform,
    root: Option<PathBuf>,
    dev: &Path,
    note: &mut dyn FnMut(&str),
) -> Result<PathBuf, String> {
    let name = p.sdk_name();
    let mut ignored = None;
    if let Some(s) = root {
        match sdk_platform(&s) {
            Some(q) if q != p => {
                note(&format!(
                    "ignoring SDKROOT={}: it is a {} SDK, and the target needs {name}.sdk",
                    s.display(),
                    q.sdk_name()
                ));
                ignored = Some(s);
            }
            _ => return Ok(s),
        }
    }
    // Xcode keeps it in the platform, the command line tools at the top.
    let found = [
        format!("Platforms/{name}.platform/Developer/SDKs/{name}.sdk"),
        format!("SDKs/{name}.sdk"),
    ]
    .iter()
    .map(|s| dev.join(s))
    .find(|s| s.is_dir());
    found.ok_or_else(|| {
        let unset = ignored.map_or_else(String::new, |s| {
            format!(" (SDKROOT={} is another platform's SDK)", s.display())
        });
        match p {
            Platform::MacOs => format!(
                "no macOS SDK in {}{unset}: install the Xcode command line tools, or set \
                 SDKROOT to a MacOSX.sdk",
                dev.display()
            ),
            Platform::Ios | Platform::IosSimulator => format!(
                "no {name} SDK in {}{unset}: install Xcode from the App Store, then run \
                 `sudo xcode-select -s /Applications/Xcode.app`, or set SDKROOT to an {name}.sdk",
                dev.display()
            ),
        }
    })
}

/// The platform SDK `sdk` is for: its `SDKSettings.json`'s
/// `CanonicalName` (`iphoneos26.0`), else its directory's name
/// (`iPhoneOS.sdk`); `None` when neither tells.
fn sdk_platform(sdk: &Path) -> Option<Platform> {
    let of = |n: &str| {
        let n = n.to_ascii_lowercase();
        // `iphonesimulator` first: it does not start with `iphoneos`.
        [Platform::IosSimulator, Platform::Ios, Platform::MacOs]
            .into_iter()
            .find(|q| n.starts_with(&q.sdk_name().to_ascii_lowercase()))
    };
    if let Some(n) = sdk_setting(sdk, "CanonicalName") {
        return of(&n);
    }
    let real = std::fs::canonicalize(sdk).unwrap_or_else(|_| sdk.to_owned());
    of(&real.file_name()?.to_string_lossy())
}

/// The oldest iOS Nabi builds for: 13.4, the first whose dyld reads
/// chained fixups, which Apple's linker writes for an app from there.
pub const IOS_MIN: &str = "13.4";

/// A deployment target `v` the platform's tools take.
pub fn check_deployment(p: Platform, v: &str) -> Result<(), String> {
    if matches!(p, Platform::Ios | Platform::IosSimulator)
        && pack_version(v) < pack_version(IOS_MIN)
    {
        return Err(format!(
            "the iOS deployment target is {v}, and Nabi builds for iOS {IOS_MIN} and later: \
             write `deployment_target: {{ ios: \"{IOS_MIN}\" }}` in \
             nabi.nabion, or `IPHONEOS_DEPLOYMENT_TARGET={IOS_MIN}`, or a later version"
        ));
    }
    Ok(())
}

/// `"11.0"` as `LC_BUILD_VERSION` packs it: `xxxx.yy.zz`.
#[must_use]
pub fn pack_version(v: &str) -> u32 {
    let mut it = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let major = it.next().unwrap_or(0);
    let minor = it.next().unwrap_or(0);
    let patch = it.next().unwrap_or(0);
    (major << 16) | ((minor & 0xff) << 8) | (patch & 0xff)
}

/// `"Version":"26.0"` of the SDK's `SDKSettings.json`.
#[must_use]
pub fn sdk_version(sdk: &Path) -> Option<String> {
    sdk_setting(sdk, "Version")
}

/// String `key` at the top of the SDK's `SDKSettings.json`.
fn sdk_setting(sdk: &Path, key: &str) -> Option<String> {
    let json = std::fs::read_to_string(sdk.join("SDKSettings.json")).ok()?;
    let k = format!("\"{key}\"");
    let rest = &json[json.find(&k)? + k.len()..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

#[cfg(test)]
mod tests {
    use super::{Platform, find_sdk, pack_version};
    use std::path::{Path, PathBuf};

    #[test]
    fn ios_sdk_says_what_to_install() {
        let clt = Path::new("/nonexistent/CommandLineTools");
        let e = find_sdk(Platform::IosSimulator, None, clt, &mut |_| {}).unwrap_err();
        assert!(
            e.starts_with("no iPhoneSimulator SDK in /nonexistent/CommandLineTools: install Xcode"),
            "{e}"
        );
        assert!(
            e.contains("sudo xcode-select -s /Applications/Xcode.app"),
            "{e}"
        );
        let root = PathBuf::from("/sdk/iPhoneOS.sdk");
        assert_eq!(
            find_sdk(Platform::Ios, Some(root.clone()), clt, &mut |_| {}),
            Ok(root)
        );
    }

    /// A macOS `SDKROOT` for an iOS target is passed over, with a note,
    /// for the developer directory's iOS SDK.
    #[test]
    fn other_platform_sdkroot_is_ignored() {
        let d = std::env::temp_dir().join(format!("nabic-sdkroot-{}", std::process::id()));
        let mac = d.join("MacOSX27.0.sdk");
        let dev = d.join("Xcode.app/Contents/Developer");
        let ios = dev.join("Platforms/iPhoneOS.platform/Developer/SDKs/iPhoneOS.sdk");
        std::fs::create_dir_all(&mac).unwrap();
        std::fs::create_dir_all(&ios).unwrap();
        std::fs::write(
            mac.join("SDKSettings.json"),
            r#"{"DisplayName":"macOS 27.0","CanonicalName":"macosx27.0","Version":"27.0"}"#,
        )
        .unwrap();
        let mut notes = Vec::new();
        let got = find_sdk(Platform::Ios, Some(mac.clone()), &dev, &mut |m| {
            notes.push(m.to_owned());
        });
        assert_eq!(got, Ok(ios.clone()));
        assert_eq!(
            notes,
            [format!(
                "ignoring SDKROOT={}: it is a MacOSX SDK, and the target needs iPhoneOS.sdk",
                mac.display()
            )]
        );
        // The simulator has none: the error says what to set.
        let e = find_sdk(Platform::IosSimulator, Some(mac.clone()), &dev, &mut |_| {}).unwrap_err();
        assert!(
            e.contains("is another platform's SDK")
                && e.ends_with("or set SDKROOT to an iPhoneSimulator.sdk"),
            "{e}"
        );
        // The matching platform's, or one that does not say, is taken.
        let mut quiet = |_: &str| panic!("no note");
        assert_eq!(
            find_sdk(Platform::MacOs, Some(mac.clone()), &dev, &mut quiet),
            Ok(mac)
        );
        assert_eq!(
            find_sdk(Platform::Ios, Some(ios.clone()), &dev, &mut quiet),
            Ok(ios)
        );
        let odd = d.join("custom");
        assert_eq!(
            find_sdk(Platform::Ios, Some(odd.clone()), &dev, &mut quiet),
            Ok(odd)
        );
        assert_eq!(
            super::sdk_version(&d.join("MacOSX27.0.sdk")).as_deref(),
            Some("27.0")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ios_deployment_floor() {
        let e = super::check_deployment(Platform::Ios, "12.0").unwrap_err();
        assert!(
            e.starts_with("the iOS deployment target is 12.0, and Nabi builds for iOS 13.4")
                && e.contains("IPHONEOS_DEPLOYMENT_TARGET=13.4"),
            "{e}"
        );
        assert!(super::check_deployment(Platform::Ios, "13.3").is_err());
        assert!(super::check_deployment(Platform::Ios, "13.4").is_ok());
        assert!(super::check_deployment(Platform::IosSimulator, "17.4").is_ok());
        assert!(super::check_deployment(Platform::MacOs, "10.15").is_ok());
    }

    #[test]
    fn versions_pack() {
        assert_eq!(pack_version("15.0"), 0x000f_0000);
        assert_eq!(pack_version("11.0"), 0x000b_0000);
        assert_eq!(pack_version("17.4.1"), 0x0011_0401);
    }
}
