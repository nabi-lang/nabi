//! An `.xcframework` of static libraries: one slice per Apple platform, as
//! `xcodebuild -create-xcframework` lays it out, and the `Info.plist`
//! that lists them. Each `--crate-type=staticlib --xcframework` build adds
//! or replaces its target's slice.

use nabic_target::TargetSpec;
use nabic_target::apple::Platform;
use std::path::Path;

/// The slice of `t`: its identifier, platform and variant.
fn slice(t: &TargetSpec) -> Option<(&'static str, &'static str, Option<&'static str>)> {
    if t.arch != "aarch64" {
        return None;
    }
    Some(match Platform::of(t)? {
        Platform::MacOs => ("macos-arm64", "macos", None),
        Platform::Ios => ("ios-arm64", "ios", None),
        Platform::IosSimulator => ("ios-arm64-simulator", "ios", Some("simulator")),
    })
}

/// Put static library `lib` and the headers in `include` into the
/// `.xcframework` `xc` as `t`'s slice, then rewrite its `Info.plist`.
pub fn add(xc: &Path, t: &TargetSpec, lib: &Path, include: &Path) -> Result<(), String> {
    let (id, ..) = slice(t).ok_or_else(|| {
        format!(
            "`--xcframework`: {} has no XCFramework slice; build for `aarch64-apple-ios`, \
             `aarch64-apple-ios-sim` or `aarch64-apple-darwin`",
            t.name
        )
    })?;
    let io = |e: std::io::Error, p: &Path| format!("cannot write {}: {e}", p.display());
    let dir = xc.join(id);
    let _ = std::fs::remove_dir_all(&dir);
    let headers = dir.join("Headers");
    std::fs::create_dir_all(&headers).map_err(|e| io(e, &headers))?;
    let file = lib.file_name().unwrap_or_default();
    std::fs::copy(lib, dir.join(file)).map_err(|e| io(e, &dir))?;
    for e in std::fs::read_dir(include).map_err(|e| io(e, include))? {
        let p = e.map_err(|e| io(e, include))?.path();
        if p.is_file() {
            std::fs::copy(&p, headers.join(p.file_name().unwrap_or_default()))
                .map_err(|e| io(e, &headers))?;
        }
    }
    let plist = xc.join("Info.plist");
    std::fs::write(&plist, info_plist(xc, &file.to_string_lossy())).map_err(|e| io(e, &plist))
}

/// The `Info.plist` listing the slices now in `xc`, each holding `lib`.
fn info_plist(xc: &Path, lib: &str) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\t<key>AvailableLibraries</key>\n\t<array>\n",
    );
    for (id, platform, variant) in [
        ("ios-arm64", "ios", None),
        ("ios-arm64-simulator", "ios", Some("simulator")),
        ("macos-arm64", "macos", None),
    ] {
        if !xc.join(id).join(lib).is_file() {
            continue;
        }
        s.push_str("\t\t<dict>\n");
        for (k, v) in [
            ("BinaryPath", lib),
            ("HeadersPath", "Headers"),
            ("LibraryIdentifier", id),
            ("LibraryPath", lib),
        ] {
            s.push_str(&format!(
                "\t\t\t<key>{k}</key>\n\t\t\t<string>{v}</string>\n"
            ));
        }
        s.push_str(
            "\t\t\t<key>SupportedArchitectures</key>\n\t\t\t<array>\n\t\t\t\t<string>arm64</string>\n\t\t\t</array>\n",
        );
        s.push_str(&format!(
            "\t\t\t<key>SupportedPlatform</key>\n\t\t\t<string>{platform}</string>\n"
        ));
        if let Some(v) = variant {
            s.push_str(&format!(
                "\t\t\t<key>SupportedPlatformVariant</key>\n\t\t\t<string>{v}</string>\n"
            ));
        }
        s.push_str("\t\t</dict>\n");
    }
    s.push_str(
        "\t</array>\n\t<key>CFBundlePackageType</key>\n\t<string>XFWK</string>\n\
         \t<key>XCFrameworkFormatVersion</key>\n\t<string>1.0</string>\n</dict>\n</plist>\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::add;
    use nabic_target::TargetSpec;

    /// Two builds make one `.xcframework` with a device and a simulator
    /// slice; building one again replaces its slice.
    #[test]
    fn device_and_simulator_slices() {
        let dir = nabic_core::tmp::test_dir("xcframework");
        let inc = dir.join("include");
        std::fs::create_dir_all(&inc).unwrap();
        std::fs::write(inc.join("calc.h"), "int calc_add(int, int);\n").unwrap();
        std::fs::write(inc.join("module.modulemap"), "module calc {}\n").unwrap();
        let lib = dir.join("libcalc.a");
        std::fs::write(&lib, "!<arch>\n").unwrap();
        let xc = dir.join("calc.xcframework");
        for t in [
            "aarch64-apple-ios",
            "aarch64-apple-ios-sim",
            "aarch64-apple-ios",
        ] {
            add(&xc, &TargetSpec::builtin(t).unwrap(), &lib, &inc).unwrap();
        }
        assert!(xc.join("ios-arm64/libcalc.a").is_file());
        assert!(
            xc.join("ios-arm64-simulator/Headers/module.modulemap")
                .is_file()
        );
        let plist = std::fs::read_to_string(xc.join("Info.plist")).unwrap();
        assert_eq!(plist.matches("<key>LibraryIdentifier</key>").count(), 2);
        assert!(plist.contains("<string>ios-arm64-simulator</string>"));
        assert!(
            plist.contains("<key>SupportedPlatformVariant</key>\n\t\t\t<string>simulator</string>")
        );
        let e = add(
            &xc,
            &TargetSpec::builtin("x86_64-unknown-linux-gnu").unwrap(),
            &lib,
            &inc,
        )
        .unwrap_err();
        assert!(e.contains("has no XCFramework slice"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
