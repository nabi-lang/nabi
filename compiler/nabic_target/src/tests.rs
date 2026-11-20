use crate::apple::Platform;
use crate::{BUILTIN, CAbi, HOST, Libc, Linker, ObjectFormat, TargetSpec};

fn t(name: &str) -> TargetSpec {
    TargetSpec::builtin(name).unwrap()
}

/// Every built-in spec prints as NABION and reads back the same, under
/// its own name; its hash is stable and its own.
#[test]
fn builtins_round_trip() {
    let mut hashes = Vec::new();
    for name in BUILTIN {
        let spec = t(name);
        assert_eq!(spec.name, *name);
        let text = spec.to_nabion();
        let back =
            TargetSpec::from_nabion(&text, "t.nabion", name).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, spec, "{name}");
        assert_eq!(back.to_nabion(), text, "{name}");
        assert_eq!(back.hash(), spec.hash(), "{name}");
        hashes.push(spec.hash());
    }
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(hashes.len(), BUILTIN.len());
    assert!(TargetSpec::builtin("x86_64-unknown-linux-gnux32").is_none());
    assert!(BUILTIN.contains(&HOST), "{HOST}");
}

/// A copy under another name differs only by name (and hash).
#[test]
fn renamed_copy() {
    let a = t("x86_64-unknown-linux-gnu");
    let b = TargetSpec::from_nabion(&a.to_nabion(), "my.nabion", "my").unwrap();
    assert_eq!(b.name, "my");
    assert_ne!(a.hash(), b.hash());
    assert_eq!(
        TargetSpec {
            name: a.name.clone(),
            ..b
        },
        a
    );
}

#[test]
fn custom_spec_errors() {
    let text = t("wasm32-wasip2").to_nabion();
    let unknown = text.replacen("TargetSpec {", "TargetSpec {\n    colour: \"red\",", 1);
    let e = TargetSpec::from_nabion(&unknown, "x.nabion", "x").unwrap_err();
    assert!(
        e.contains("unknown field `colour`") && e.contains("x.nabion:2:5"),
        "{e}"
    );
    let missing = text.replacen("    threads: false,\n", "", 1);
    let e = TargetSpec::from_nabion(&missing, "x.nabion", "x").unwrap_err();
    assert!(e.contains("missing field `threads`"), "{e}");
    let bad = text.replacen("Linker::WasmComponent", "Linker::Gold", 1);
    let e = TargetSpec::from_nabion(&bad, "x.nabion", "x").unwrap_err();
    assert!(e.contains("expected one of Linker::MachO"), "{e}");
    let bad = text.replacen("\"little\"", "\"middle\"", 1);
    let e = TargetSpec::from_nabion(&bad, "x.nabion", "x").unwrap_err();
    assert!(e.contains("`endian` must be"), "{e}");
    let e = TargetSpec::resolve(Some("nope-nope")).unwrap_err();
    assert!(e.contains("unknown target `nope-nope`"), "{e}");
    let e = TargetSpec::resolve(Some("/no/such/t.nabion")).unwrap_err();
    assert!(e.contains("cannot read target /no/such/t.nabion"), "{e}");
}

#[test]
fn builtin_properties() {
    let i686 = t("i686-unknown-linux-gnu");
    assert_eq!(
        (
            i686.arch.as_str(),
            i686.pointer_width,
            i686.wide_align,
            i686.c_abi
        ),
        ("x86", 32, 4, CAbi::X86SysV)
    );
    let w = t("i686-pc-windows-msvc");
    assert_eq!(
        (w.wide_align, w.c_abi, w.c_symbol_prefix.as_str()),
        (8, CAbi::X86Win, "_")
    );
    assert_eq!(w.coff_machine(), Some(0x14c));
    assert_eq!(w.runtime_lib(), "nabir.lib");
    let arm = t("armv7-unknown-linux-gnueabihf");
    assert_eq!(
        (arm.arch.as_str(), arm.env.as_str(), arm.c_abi),
        ("arm", "gnu", CAbi::ArmHf)
    );
    let mac = t("aarch64-apple-darwin");
    assert_eq!(mac.c_triple(), "arm64-apple-macosx11.0");
    assert_eq!(
        (mac.os.as_str(), mac.runtime_lib().as_str()),
        ("macos", "libnabir.a")
    );
    let musl = t("aarch64-unknown-linux-musl");
    assert_eq!(
        (musl.c_target.as_str(), musl.libc),
        ("aarch64-unknown-linux-musl", Libc::Musl)
    );
    assert!(!musl.dynamic_linking && musl.native_backend);
    let arm_musl = t("armv7-unknown-linux-musleabihf");
    assert_eq!(
        (
            arm_musl.arch.as_str(),
            arm_musl.env.as_str(),
            arm_musl.c_abi
        ),
        ("arm", "musl", CAbi::ArmHf)
    );
    assert_eq!(
        (
            arm_musl.libc,
            arm_musl.pointer_width,
            arm_musl.features.as_str()
        ),
        (Libc::Musl, 32, arm.features.as_str())
    );
    assert!(!arm_musl.dynamic_linking && arm_musl.native_backend);
    let b = t("wasm32-unknown-unknown");
    assert!(b.reactor());
    assert_eq!((b.os.as_str(), b.family.as_str()), ("unknown", "wasm"));
    assert_eq!(t("wasm32-wasip2").linker, Linker::WasmComponent);
    assert!(t("wasm32-wasip1-threads").threads && !t("wasm32-wasip2").threads);
    assert_eq!(t("s390x-unknown-linux-gnu").endian, "big");
}

/// A near-miss target name (Rust's older spellings) is unknown, with the
/// closest built-in one suggested.
#[test]
fn target_old_name_suggests() {
    for (old, new) in [
        ("wasm32-wasi", "wasm32-wasip2"),
        ("wasm32-wasi-threads", "wasm32-wasip1-threads"),
        ("riscv32imac-unknown-none", "riscv32imac-unknown-none-elf"),
    ] {
        assert!(TargetSpec::builtin(old).is_none(), "{old}");
        let e = TargetSpec::resolve(Some(old)).unwrap_err();
        assert!(
            e.starts_with(&format!("unknown target `{old}`; did you mean `{new}`?")),
            "{e}"
        );
    }
}

/// A target that is not supported says so, and what to use instead
/// where there is something.
#[test]
fn target_unavailable_says_why() {
    if crate::in_progress_on() {
        return;
    }
    for (name, want) in [
        (
            "x86_64-apple-darwin",
            "target `x86_64-apple-darwin` is not supported (Intel Macs); use `aarch64-apple-darwin`",
        ),
        (
            "armv7-linux-androideabi",
            "target `armv7-linux-androideabi` is not supported (32-bit Android); \
             `aarch64-linux-android` is in progress, for now build for the emulator with \
             `--target x86_64-linux-android`",
        ),
        (
            "thumbv7em-none-eabihf",
            "target `thumbv7em-none-eabihf` is not supported (Cortex-M); `nabic --print \
             target-list`",
        ),
    ] {
        assert!(TargetSpec::builtin(name).is_none(), "{name}");
        let e = TargetSpec::resolve(Some(name)).unwrap_err();
        assert!(e.starts_with(want), "{e}");
    }
}

/// An in-progress target is built in, but `resolve` refuses it without
/// the opt-in, with what to build now.
#[test]
fn in_progress_targets_are_refused() {
    for t in crate::builtin::BUILTIN {
        assert!(TargetSpec::builtin(t).is_some(), "{t}");
        assert!(TargetSpec::resolve_any(Some(t)).is_ok(), "{t}");
    }
    if crate::in_progress_on() {
        return;
    }
    let e = TargetSpec::resolve(Some("aarch64-apple-ios")).unwrap_err();
    assert_eq!(
        e,
        "target `aarch64-apple-ios` (iOS devices) is in progress: it is not tested on real \
         hardware yet and comes in a later release; for now build for the simulator with \
         `--target aarch64-apple-ios-sim`"
    );
    let e = TargetSpec::resolve(Some("s390x-unknown-linux-gnu")).unwrap_err();
    assert!(
        e.ends_with("; no supported target stands in for it yet"),
        "{e}"
    );
    assert!(crate::available().all(|t| !crate::in_progress(t)));
    assert_eq!(crate::available().count(), 17);
}

/// The armv7 targets are supported: `--target` takes them without the
/// opt-in.
#[test]
fn armv7_is_supported() {
    for arm in [
        "armv7-unknown-linux-gnueabihf",
        "armv7-unknown-linux-musleabihf",
    ] {
        assert!(!crate::in_progress(arm), "{arm}");
        assert!(crate::available().any(|t| t == arm), "{arm}");
        assert!(TargetSpec::resolve(Some(arm)).is_ok(), "{arm}");
    }
}

#[test]
fn android_targets() {
    for (name, arch) in [
        ("aarch64-linux-android", "aarch64"),
        ("x86_64-linux-android", "x86_64"),
    ] {
        let a = t(name);
        assert_eq!(
            (a.arch.as_str(), a.os.as_str(), a.env.as_str(), a.libc),
            (arch, "android", "", Libc::Bionic)
        );
        assert_eq!(a.c_triple(), format!("{name}29"));
        assert_eq!(a.dynamic_loader, "/system/bin/linker64");
        assert!(a.dynamic_linking && a.native_backend && !a.repl && !a.libc.shipped());
    }
    assert_eq!(t("x86_64-linux-android").cpu, "x86-64-v2");
}

/// iOS: the device and the simulator differ in `abi`, platform and CPU
/// baseline; neither runs the REPL.
#[test]
fn ios_targets() {
    let dev = t("aarch64-apple-ios");
    let sim = t("aarch64-apple-ios-sim");
    for x in [&dev, &sim] {
        assert_eq!(
            (x.os.as_str(), x.vendor.as_str(), x.object_format, x.c_abi),
            ("ios", "apple", ObjectFormat::MachO, CAbi::Aarch64)
        );
        assert_eq!((x.deployment_target(), x.libc), ("13.4", Libc::LibSystem));
        assert!(x.native_backend && x.threads && !x.repl);
        assert_eq!(x.runtime_lib(), "libnabir.a");
        assert_eq!(x.dylib("m"), "libm.dylib");
    }
    assert_eq!((dev.abi.as_str(), sim.abi.as_str()), ("", "sim"));
    assert_eq!(dev.c_triple(), "arm64-apple-ios13.4");
    assert_eq!(sim.c_triple(), "arm64-apple-ios13.4-simulator");
    assert_eq!(Platform::of(&dev), Some(Platform::Ios));
    assert_eq!(Platform::of(&sim), Some(Platform::IosSimulator));
    assert_eq!(
        Platform::of(&t("aarch64-apple-darwin")).map(Platform::id),
        Some(1)
    );
    assert_eq!(Platform::of(&t("x86_64-unknown-linux-gnu")), None);
    assert_eq!((Platform::Ios.id(), Platform::IosSimulator.id()), (2, 7));
    assert_eq!(Platform::IosSimulator.name(), "ios-simulator");
    // The device baseline is ARMv8.0: no LSE, as on A9 phones.
    assert!(!dev.cpu_features().has("lse"), "{:?}", dev.cpu);
    assert!(sim.cpu_features().has("lse"));
}
