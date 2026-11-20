//! The COFF linker on a small hand-written i386 object: a PE32 image
//! with its 32-bit headers, import thunks and table, `HIGHLOW` base
//! relocations and TLS directory in place.

use nabic_target::TargetSpec;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(&format!("link-coff-{name}"))
}

const fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// A section of the object: name, characteristics, contents,
/// relocations `(offset, symbol index, type)`.
type Sect<'a> = (&'a str, u32, Vec<u8>, Vec<(u32, u32, u16)>);

/// A symbol: name, value, section number (0 undefined, -1 absolute),
/// class.
type Sym<'a> = (&'a str, u32, i16, u8);

fn object(sects: &[Sect<'_>], syms: &[Sym<'_>]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0x14cu16.to_le_bytes());
    b.extend_from_slice(&(sects.len() as u16).to_le_bytes());
    b.extend_from_slice(&[0; 16]);
    let mut at = 20 + 40 * sects.len();
    let mut body = Vec::new();
    for (name, chars, data, relocs) in sects {
        let mut n = [0u8; 8];
        n[..name.len()].copy_from_slice(name.as_bytes());
        b.extend_from_slice(&n);
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&(at as u32).to_le_bytes());
        let rel = at + data.len();
        b.extend_from_slice(&(if relocs.is_empty() { 0 } else { rel as u32 }).to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&(relocs.len() as u16).to_le_bytes());
        b.extend_from_slice(&[0; 2]);
        b.extend_from_slice(&chars.to_le_bytes());
        body.extend_from_slice(data);
        for (off, sym, ty) in relocs {
            body.extend_from_slice(&off.to_le_bytes());
            body.extend_from_slice(&sym.to_le_bytes());
            body.extend_from_slice(&ty.to_le_bytes());
        }
        at = rel + 10 * relocs.len();
    }
    b.extend_from_slice(&body);
    let symoff = b.len() as u32;
    b[8..12].copy_from_slice(&symoff.to_le_bytes());
    b[12..16].copy_from_slice(&(syms.len() as u32).to_le_bytes());
    let mut strs = Vec::new();
    for (name, value, sect, class) in syms {
        if name.len() <= 8 {
            let mut n = [0u8; 8];
            n[..name.len()].copy_from_slice(name.as_bytes());
            b.extend_from_slice(&n);
        } else {
            b.extend_from_slice(&[0; 4]);
            b.extend_from_slice(&(strs.len() as u32 + 4).to_le_bytes());
            strs.extend_from_slice(name.as_bytes());
            strs.push(0);
        }
        b.extend_from_slice(&value.to_le_bytes());
        b.extend_from_slice(&sect.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&[*class, 0]);
    }
    b.extend_from_slice(&(strs.len() as u32 + 4).to_le_bytes());
    b.extend_from_slice(&strs);
    b
}

const DIR32: u16 = 6;
const REL32: u16 = 0x14;

/// `_main`: reads `_ptrs` (a pointer to itself), calls `GetTickCount`
/// through its IAT slot and `Sleep` through a thunk; the TLS directory
/// `__tls_used` over 8-aligned TLS data.
fn main_object() -> Vec<u8> {
    let code = vec![
        0xa1, 0, 0, 0, 0, // mov eax, [_ptrs]
        0xff, 0x15, 0, 0, 0, 0, // call [__imp__GetTickCount@0]
        0x6a, 0x01, // push 1
        0xe8, 0, 0, 0, 0, // call _Sleep@4
        0x31, 0xc0, // xor eax, eax
        0xc3, // ret
    ];
    // Start, end (8 past it), index, callbacks.
    let mut dir = vec![0; 24];
    dir[4] = 8;
    let sects = [
        (
            ".text",
            0x6050_0020,
            code,
            vec![(1, 3, DIR32), (7, 4, DIR32), (14, 5, REL32)],
        ),
        (".data", 0xc030_0040, vec![0; 4], vec![(0, 2, DIR32)]),
        (".tls$", 0xc040_0040, vec![0; 8], vec![]),
        (
            ".rdata$T",
            0x4030_0040,
            dir,
            vec![(0, 8, DIR32), (4, 8, DIR32), (8, 3, DIR32), (12, 3, DIR32)],
        ),
    ];
    let syms = [
        (".text", 0, 1, 3),
        (".data", 0, 2, 3),
        ("_main", 0, 1, 2),
        ("_ptrs", 0, 2, 2),
        ("__imp__GetTickCount@0", 0, 0, 2),
        ("_Sleep@4", 0, 0, 2),
        ("@feat.00", 1, -1, 3),
        ("__tls_used", 0, 4, 2),
        (".tls$", 0, 3, 3),
    ];
    object(&sects, &syms)
}

/// The file offset of `rva` in PE image `b` with section headers at `sh`.
fn file_at(b: &[u8], sh: usize, n: usize, rva: u32) -> usize {
    for k in 0..n {
        let s = sh + 40 * k;
        let (va, size, raw) = (u32_at(b, s + 12), u32_at(b, s + 8), u32_at(b, s + 20));
        if (va..va + size).contains(&rva) {
            return (raw + rva - va) as usize;
        }
    }
    panic!("RVA {rva:#x} in no section");
}

#[test]
fn links_a_pe32_image() {
    let dir = scratch("pe32");
    let t = TargetSpec::builtin("i686-pc-windows-msvc").unwrap();
    let obj = dir.join("main.obj");
    std::fs::write(&obj, main_object()).unwrap();
    let lib = dir.join("kernel32.lib");
    let def = "LIBRARY KERNEL32.dll\nEXPORTS\nGetTickCount@0\nSleep@4\n";
    nabild::import_library(&t, def, &lib).unwrap();
    let exe = dir.join("main.exe");
    let args = [
        obj.into_os_string(),
        lib.into_os_string(),
        "/subsystem:console".into(),
        "/entry:main".into(),
        "/stack:8388608".into(),
        "/include:__tls_used".into(),
        "/Brepro".into(),
    ];
    nabild::link_executable(&t, &args, &exe, true, None).unwrap();
    let b = std::fs::read(&exe).unwrap();

    let pe = u32_at(&b, 0x3c) as usize;
    assert_eq!(&b[pe..pe + 4], b"PE\0\0");
    assert_eq!(u16_at(&b, pe + 4), 0x14c);
    // Optional header of 224 bytes; executable, large address aware,
    // 32-bit.
    assert_eq!(u16_at(&b, pe + 20), 224);
    assert_eq!(u16_at(&b, pe + 22), 0x0122);
    let opt = pe + 24;
    assert_eq!(u16_at(&b, opt), 0x10b);
    assert_eq!(u32_at(&b, opt + 28), 0x40_0000);
    // Dynamic base, NX, terminal server aware; no high entropy VA.
    assert_eq!(u16_at(&b, opt + 70), 0x8140);
    assert_eq!(u32_at(&b, opt + 72), 8 << 20);
    assert_eq!(u32_at(&b, opt + 80), 0x10_0000);
    assert_eq!(u32_at(&b, opt + 92), 16);
    let dirs = opt + 96;
    let dir_at = |k: usize| (u32_at(&b, dirs + 8 * k), u32_at(&b, dirs + 8 * k + 4));
    let nsects = usize::from(u16_at(&b, pe + 6));
    let sh = opt + 224;
    let at = |rva: u32| file_at(&b, sh, nsects, rva);

    // The entry `_main`, its absolute addresses rebased.
    let entry = u32_at(&b, opt + 16);
    let code = at(entry);
    let va = |x: u32| x - 0x40_0000;
    let ptrs = va(u32_at(&b, code + 1));
    assert_eq!(u32_at(&b, at(ptrs)), 0x40_0000 + entry);
    // The IAT: two 4-byte slots and the terminator.
    let (iat, iat_size) = dir_at(12);
    assert_eq!(iat_size, 12);
    let slot = va(u32_at(&b, code + 7));
    assert!((iat..iat + 8).contains(&slot));
    // `Sleep`'s thunk: jmp [slot].
    let thunk = (i64::from(entry) + 18 + i64::from(u32_at(&b, code + 14) as i32)) as u32;
    let th = at(thunk);
    assert_eq!(b[th..th + 2], [0xff, 0x25]);
    assert!((iat..iat + 8).contains(&va(u32_at(&b, th + 2))));
    // The hint / name entries of both, by name.
    let names: Vec<String> = (0..2)
        .map(|k| {
            let hn = u32_at(&b, at(iat + 4 * k));
            assert_eq!(hn & 0x8000_0000, 0);
            let s = at(hn) + 2;
            let end = b[s..].iter().position(|&c| c == 0).unwrap();
            String::from_utf8_lossy(&b[s..s + end]).into_owned()
        })
        .collect();
    assert_eq!(names, ["GetTickCount", "Sleep"]);

    // Base relocations: every absolute address, all `HIGHLOW`.
    let (reloc, reloc_size) = dir_at(5);
    let mut fixups = Vec::new();
    let mut p = at(reloc);
    let end = p + reloc_size as usize;
    while p < end {
        let (page, size) = (u32_at(&b, p), u32_at(&b, p + 4) as usize);
        for k in (8..size).step_by(2) {
            let e = u16_at(&b, p + k);
            if e != 0 {
                assert_eq!(e >> 12, 3);
                fixups.push(page + u32::from(e & 0xfff));
            }
        }
        p += size;
    }
    fixups.sort_unstable();
    let mut want = vec![entry + 1, entry + 7, thunk + 2, ptrs];
    // The TLS directory's start, end, index and callbacks.
    let (tls, tls_size) = dir_at(9);
    assert_eq!(tls_size, 24);
    want.extend((0..4).map(|k| tls + 4 * k));
    want.sort_unstable();
    assert_eq!(fixups, want);
    // Its alignment: the TLS data's 8.
    assert_eq!(u32_at(&b, at(tls) + 20), 0x0040_0000);
}

#[test]
fn rejects_safeseh() {
    let dir = scratch("safeseh");
    let t = TargetSpec::builtin("i686-pc-windows-msvc").unwrap();
    let obj = dir.join("main.obj");
    std::fs::write(&obj, main_object()).unwrap();
    let args = [
        obj.into_os_string(),
        "/safeseh".into(),
        "/subsystem:console".into(),
        "/entry:main".into(),
    ];
    let e = nabild::link_executable(&t, &args, &dir.join("a.exe"), true, None).unwrap_err();
    assert!(e.contains("/safeseh:no"), "{e}");
}
