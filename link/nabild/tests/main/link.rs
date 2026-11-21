//! The Mach-O linker and the archive writer on small hand-written
//! objects: an executable that runs, is signed, and resolves through an
//! archive and libSystem; iOS executables against stub SDKs.
#![cfg(all(target_vendor = "apple", target_arch = "aarch64"))]

use nabic_target::TargetSpec;
use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(&format!("link-{name}"))
}

fn u32s(b: &mut Vec<u8>, v: &[u32]) {
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
}

fn name16(b: &mut Vec<u8>, n: &str) {
    let mut f = [0u8; 16];
    f[..n.len()].copy_from_slice(n.as_bytes());
    b.extend_from_slice(&f);
}

/// An arm64 macOS object: `__text` holding `code`, symbols `(name, type,
/// value)` and `(offset, symbol, type, pcrel)` relocations.
fn object(code: &[u8], syms: &[(&str, u8, u64)], relocs: &[(u32, u32, u32, bool)]) -> Vec<u8> {
    object_for(1, 0x000b_0000, code, syms, relocs)
}

/// [`object`] for `PLATFORM_*` `platform`, minimum OS `minos`.
fn object_for(
    platform: u32,
    minos: u32,
    code: &[u8],
    syms: &[(&str, u8, u64)],
    relocs: &[(u32, u32, u32, bool)],
) -> Vec<u8> {
    let cmds = 152 + 24 + 24;
    let at = 32 + cmds;
    let len = code.len() as u32;
    let reloff = (at + len).next_multiple_of(8);
    let symoff = reloff + 8 * relocs.len() as u32;
    let mut strs = vec![0u8];
    let mut nlist = Vec::new();
    for (n, ty, v) in syms {
        u32s(&mut nlist, &[strs.len() as u32]);
        strs.extend_from_slice(n.as_bytes());
        strs.push(0);
        nlist.extend_from_slice(&[*ty, u8::from(*ty & 0xe == 0xe), 0, 0]);
        nlist.extend_from_slice(&v.to_le_bytes());
    }
    let stroff = symoff + nlist.len() as u32;
    let mut b = Vec::new();
    u32s(
        &mut b,
        &[0xfeed_facf, 0x0100_000c, 0, 1, 3, cmds, 0x2000, 0],
    );
    u32s(&mut b, &[0x19, 152]);
    name16(&mut b, "");
    for v in [0, u64::from(len), u64::from(at), u64::from(len)] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    u32s(&mut b, &[7, 7, 1, 0]);
    name16(&mut b, "__text");
    name16(&mut b, "__TEXT");
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&u64::from(len).to_le_bytes());
    u32s(
        &mut b,
        &[
            at,
            2,
            if relocs.is_empty() { 0 } else { reloff },
            relocs.len() as u32,
            0x8000_0400,
            0,
            0,
            0,
        ],
    );
    u32s(&mut b, &[0x32, 24, platform, minos, 0, 0]);
    u32s(
        &mut b,
        &[
            0x2,
            24,
            symoff,
            syms.len() as u32,
            stroff,
            strs.len() as u32,
        ],
    );
    b.extend_from_slice(code);
    b.resize(reloff as usize, 0);
    for &(off, sym, ty, pcrel) in relocs {
        u32s(
            &mut b,
            &[
                off,
                sym | (u32::from(pcrel) << 24) | (2 << 25) | (1 << 27) | (ty << 28),
            ],
        );
    }
    b.extend_from_slice(&nlist);
    b.extend_from_slice(&strs);
    b
}

/// `_main` returning 42.
fn main_object() -> Vec<u8> {
    // mov w0, #42; ret
    object(
        &[0x40, 0x05, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6],
        &[("_main", 0x0f, 0)],
        &[],
    )
}

#[test]
fn archive_links_and_runs() {
    let dir = scratch("archive");
    let host = TargetSpec::host();
    let o = dir.join("main.o");
    std::fs::write(&o, main_object()).unwrap();
    // Through an archive, as a library build hands it to the link.
    let a = dir.join("libmain.a");
    nabild::write_archive(host, &a, &[&o]).unwrap();
    let exe = dir.join("main");
    let l = format!("-L{}", dir.display());
    nabild::link_executable(host, &[l.as_str(), "-lmain"], &exe, true, None).unwrap();
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(run.status.code(), Some(42));
    let sig = Command::new("codesign")
        .arg("-v")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        sig.status.success(),
        "{}",
        String::from_utf8_lossy(&sig.stderr)
    );
    let e = nabild::link_executable(host, &["-lno_such_lib_xyz"], &dir.join("x"), true, None)
        .unwrap_err();
    assert!(e.contains("no_such_lib_xyz"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `_main` calls libSystem's `_exit(7)` through a stub; an undefined
/// symbol is an error naming it and its object.
#[test]
fn binds_libsystem_and_reports_undefined() {
    let dir = scratch("bind");
    let host = TargetSpec::host();
    // mov w0, #7; bl _exit
    let code = [0xe0, 0x00, 0x80, 0x52, 0x00, 0x00, 0x00, 0x94];
    let syms = [("_main", 0x0f, 0), ("_exit", 0x01, 0)];
    let o = dir.join("exit.o");
    std::fs::write(&o, object(&code, &syms, &[(4, 1, 2, true)])).unwrap();
    let exe = dir.join("exit");
    nabild::link_executable(host, &[&o], &exe, true, None).unwrap();
    assert_eq!(Command::new(&exe).status().unwrap().code(), Some(7));
    let syms = [("_main", 0x0f, 0), ("_nabi_no_such_fn", 0x01, 0)];
    let o = dir.join("undef.o");
    std::fs::write(&o, object(&code, &syms, &[(4, 1, 2, true)])).unwrap();
    let e = nabild::link_executable(host, &[&o], &dir.join("u"), true, None).unwrap_err();
    assert!(
        e.contains("undefined symbol: _nabi_no_such_fn") && e.contains("undef.o"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stub SDK under `dir` whose libSystem exports `_exit` for `target`
/// (`arm64-ios-simulator`, ..).
fn stub_sdk(dir: &std::path::Path, target: &str) -> PathBuf {
    let sdk = dir.join(format!("{target}.sdk"));
    std::fs::create_dir_all(sdk.join("usr/lib")).unwrap();
    let tbd = format!(
        "--- !tapi-tbd\ntbd-version:     4\ntargets:         [ {target} ]\n\
         install-name:    '/usr/lib/libSystem.B.dylib'\ncurrent-version: 1351\n\
         exports:\n  - targets:         [ {target} ]\n    symbols:         [ _exit ]\n...\n"
    );
    std::fs::write(sdk.join("usr/lib/libSystem.tbd"), tbd).unwrap();
    sdk
}

/// The `(cmd, payload)` load commands of Mach-O `b`.
fn load_commands(b: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let mut at = 32;
    (0..u(16))
        .map(|_| {
            let (cmd, size) = (u(at), u(at + 4) as usize);
            let lc = (cmd, b[at + 8..at + size].to_vec());
            at += size;
            lc
        })
        .collect()
}

/// iOS device and simulator executables: `LC_BUILD_VERSION` names the
/// platform and the minimum OS, libSystem binds from the platform's
/// stubs, and stubs of another platform do not serve the link.
#[test]
fn ios_executables_name_their_platform() {
    let dir = scratch("ios");
    // mov w0, #7; bl _exit
    let code = [0xe0, 0x00, 0x80, 0x52, 0x00, 0x00, 0x00, 0x94];
    let syms = [("_main", 0x0f, 0), ("_exit", 0x01, 0)];
    for (platform, name, tbd) in [
        (2, "ios", "arm64-ios"),
        (7, "ios-simulator", "arm64-ios-simulator"),
    ] {
        let o = dir.join(format!("{name}.o"));
        std::fs::write(
            &o,
            object_for(platform, 0x000d_0400, &code, &syms, &[(4, 1, 2, true)]),
        )
        .unwrap();
        let sdk = stub_sdk(&dir, tbd);
        let exe = dir.join(name);
        let args = |sdk: &std::path::Path| -> Vec<std::ffi::OsString> {
            let mut a: Vec<std::ffi::OsString> =
                ["-arch", "arm64", "-platform_version", name, "13.4", "27.2"]
                    .map(Into::into)
                    .to_vec();
            a.extend([
                "-syslibroot".into(),
                sdk.into(),
                "-o".into(),
                exe.clone().into(),
            ]);
            a.extend([o.clone().into(), "-lSystem".into()]);
            a
        };
        let opts = nabild::macho::parse_args(&args(&sdk)).unwrap();
        nabild::macho::link_with(&opts, None).unwrap();
        let image = std::fs::read(&exe).unwrap();
        let lcs = load_commands(&image);
        let build = lcs
            .iter()
            .find(|(c, _)| *c == 0x32)
            .expect("LC_BUILD_VERSION");
        let word = |i: usize| u32::from_le_bytes(build.1[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(
            (word(0), word(1), word(2)),
            (platform, 0x000d_0400, 0x001b_0200),
            "{name}"
        );
        let dylib = lcs.iter().find(|(c, _)| *c == 0xc).expect("LC_LOAD_DYLIB");
        assert!(
            dylib.1[16..].starts_with(b"/usr/lib/libSystem.B.dylib\0"),
            "{name}"
        );
        assert!(
            lcs.iter().any(|(c, _)| *c == 0x1d),
            "{name}: LC_CODE_SIGNATURE"
        );
        // A macOS object does not go into an iOS link.
        let mac_o = dir.join("mac.o");
        std::fs::write(&mac_o, object(&code, &syms, &[(4, 1, 2, true)])).unwrap();
        let mut mac_args = args(&sdk);
        let at = mac_args.iter().position(|x| *x == o.as_os_str()).unwrap();
        mac_args[at] = mac_o.clone().into();
        let e = nabild::macho::link_with(&nabild::macho::parse_args(&mac_args).unwrap(), None)
            .unwrap_err();
        assert!(
            e.contains("mac.o: built for macOS, but this link is for")
                && e.contains("build it for the same target"),
            "{name}: {e}"
        );
        // macOS stubs do not serve an iOS link.
        let mac = stub_sdk(&dir, "arm64-macos");
        let opts = nabild::macho::parse_args(&args(&mac)).unwrap();
        let e = nabild::macho::link_with(&opts, None).unwrap_err();
        assert!(e.contains("undefined symbol: _exit"), "{name}: {e}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An FDE whose CIE pointer is a SUBTRACTOR pair, as some `ld -r` output
/// has it and the assembler writes it across atoms: the inline part is
/// only the addend, the pointer is the pair's difference. The
/// alignment between the records keeps the assembler from folding it.
#[test]
fn eh_frame_cie_pointer_relocation() {
    let dir = scratch("eh-cie-reloc");
    let s = dir.join("eh.s");
    std::fs::write(
        &s,
        "\t.section __TEXT,__text,regular,pure_instructions\n\
         \t.globl _main\n\t.p2align 2\n\
         _main:\n\tmov w0, #42\n\tret\n\
         \t.section __TEXT,__eh_frame,coalesced,no_toc+strip_static_syms+live_support\n\
         cie:\n\t.long 0x10\n\t.long 0\n\t.byte 1\n\t.asciz \"zR\"\n\
         \t.byte 1, 0x78, 30, 1, 0x10, 0x0c, 31, 0\n\
         \t.p2align 2\n\
         fde:\n\t.long 0x18\n\
         Lptr:\n\t.long Lptr - cie\n\t.quad _main - .\n\t.quad 8\n\
         \t.byte 0, 0, 0, 0\n\
         \t.subsections_via_symbols\n",
    )
    .unwrap();
    let o = dir.join("eh.o");
    let ok = Command::new("cc")
        .args(["-c", "-o"])
        .arg(&o)
        .arg(&s)
        .status()
        .unwrap();
    assert!(ok.success());
    let exe = dir.join("eh");
    nabild::link_executable(TargetSpec::host(), &[&o], &exe, true, None).unwrap();
    assert_eq!(Command::new(&exe).status().unwrap().code(), Some(42));
    let _ = std::fs::remove_dir_all(&dir);
}
