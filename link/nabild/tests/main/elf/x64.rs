//! The ELF linker on a small hand-written `x86_64` object, through an
//! archive nabic writes: a static executable with its entry point and
//! segments in place, run on `x86_64` Linux.

#![allow(clippy::many_single_char_names)]

use nabic_target::TargetSpec;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(&format!("link-elf-{name}"))
}

/// A section header: name, type, flags, offset, size, link, info,
/// alignment, entry size.
type Shdr = (u32, u32, u64, usize, usize, u32, u32, u64, u64);

/// An `x86_64` object defining `_start`: exit(`code`) by syscall.
fn object(code: u8) -> Vec<u8> {
    // mov eax, 60; mov edi, code; syscall
    let text: &[u8] = &[0xb8, 0x3c, 0, 0, 0, 0xbf, code, 0, 0, 0, 0x0f, 0x05];
    let strtab = b"\0_start\0";
    let shstrtab = b"\0.text\0.symtab\0.strtab\0.shstrtab\0";
    let text_at = 64usize;
    let sym_at = (text_at + text.len()).next_multiple_of(8);
    let str_at = sym_at + 48;
    let shstr_at = str_at + strtab.len();
    let sh_at = (shstr_at + shstrtab.len()).next_multiple_of(8);
    let mut b = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
    b.resize(16, 0);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&62u16.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&(sh_at as u64).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    for v in [64u16, 0, 0, 64, 5, 4] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(text);
    b.resize(sym_at, 0);
    b.extend_from_slice(&[0; 24]);
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&[0x12, 0]);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&(text.len() as u64).to_le_bytes());
    b.extend_from_slice(strtab);
    b.extend_from_slice(shstrtab);
    b.resize(sh_at, 0);
    let sects: [Shdr; 5] = [
        (0, 0, 0, 0, 0, 0, 0, 0, 0),
        (1, 1, 6, text_at, text.len(), 0, 0, 16, 0),
        (7, 2, 0, sym_at, 48, 3, 1, 8, 24),
        (15, 3, 0, str_at, strtab.len(), 0, 0, 1, 0),
        (23, 3, 0, shstr_at, shstrtab.len(), 0, 0, 1, 0),
    ];
    for (name, ty, flags, off, size, link, info, align, entsize) in sects {
        b.extend_from_slice(&name.to_le_bytes());
        b.extend_from_slice(&ty.to_le_bytes());
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&(off as u64).to_le_bytes());
        b.extend_from_slice(&(size as u64).to_le_bytes());
        b.extend_from_slice(&link.to_le_bytes());
        b.extend_from_slice(&info.to_le_bytes());
        b.extend_from_slice(&align.to_le_bytes());
        b.extend_from_slice(&entsize.to_le_bytes());
    }
    b
}

const fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[test]
fn static_executable_from_an_archive() {
    let dir = scratch("static");
    let t = TargetSpec::builtin("x86_64-unknown-linux-musl").unwrap();
    let o = dir.join("a-member-with-a-long-name.o");
    std::fs::write(&o, object(7)).unwrap();
    let a = dir.join("libstart.a");
    nabild::write_archive(&t, &a, &[&o]).unwrap();
    let exe = dir.join("start");
    let args = [
        "-m".into(),
        "elf_x86_64".into(),
        "-static".into(),
        "--gc-sections".into(),
        "--build-id".into(),
        "-o".into(),
        exe.clone().into_os_string(),
        a.into_os_string(),
    ];
    let opts = nabild::elf::parse_args(&args).unwrap();
    nabild::elf::link(&opts).unwrap();
    let b = std::fs::read(&exe).unwrap();
    assert!(b.starts_with(b"\x7fELF\x02\x01"));
    assert_eq!(u16_at(&b, 16), 2, "ET_EXEC");
    assert_eq!(u16_at(&b, 18), 62);
    // The entry is `_start`, in the first executable segment.
    let entry = u64_at(&b, 24);
    let (phoff, phnum) = (u64_at(&b, 32) as usize, u16_at(&b, 56) as usize);
    let exec = (0..phnum)
        .map(|i| phoff + 56 * i)
        .find(|&p| u32::from_le_bytes(b[p..p + 4].try_into().unwrap()) == 1 && b[p + 4] & 1 != 0);
    let p = exec.expect("an executable PT_LOAD");
    let (vaddr, memsz) = (u64_at(&b, p + 16), u64_at(&b, p + 40));
    assert!(entry >= vaddr && entry < vaddr + memsz, "{entry:#x}");
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        let s = std::process::Command::new(&exe).status().unwrap();
        assert_eq!(s.code(), Some(7));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reports_undefined_symbols_and_bad_flags() {
    let dir = scratch("undef");
    let exe = dir.join("x");
    let o = dir.join("o.o");
    std::fs::write(&o, object(0)).unwrap();
    let args = |extra: &str| {
        vec![
            "-m".to_owned(),
            "elf_x86_64".to_owned(),
            "-static".to_owned(),
            extra.to_owned(),
            "-o".to_owned(),
            exe.display().to_string(),
            o.display().to_string(),
        ]
    };
    let e = nabild::elf::parse_args(&args("--no-such-flag")).unwrap_err();
    assert!(e.contains("--no-such-flag"), "{e}");
    // An entry point nothing defines.
    let mut opts = nabild::elf::parse_args(&args("--gc-sections")).unwrap();
    opts.entry = "nabi_no_such_entry".to_owned();
    let e = nabild::elf::link(&opts).unwrap_err();
    assert!(e.contains("undefined symbol: nabi_no_such_entry"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `-m elf_x86_64_fbsd` marks the executable FreeBSD's (`EI_OSABI`), as
/// ld.lld does; Linux's emulation leaves it System V.
#[test]
fn freebsd_emulation_sets_the_os_abi() {
    let dir = scratch("fbsd");
    let o = dir.join("o.o");
    std::fs::write(&o, object(0)).unwrap();
    for (m, abi) in [("elf_x86_64_fbsd", 9), ("elf_x86_64", 0)] {
        let exe = dir.join(m);
        let args = [
            "-m".into(),
            m.into(),
            "-static".into(),
            "-o".into(),
            exe.clone().into_os_string(),
            o.clone().into_os_string(),
        ];
        let opts = nabild::elf::parse_args(&args).unwrap();
        nabild::elf::link(&opts).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap()[7], abi, "{m}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An `x86_64` object as non-PIC code makes it: `_start` (exit 0) and a
/// pointer to it in `.rodata`, an `R_X86_64_64` there.
fn rodata_pointer_object() -> Vec<u8> {
    let text: &[u8] = &[0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05];
    let rodata = [0u8; 8];
    // r_offset 0, symbol 1 (`_start`), R_X86_64_64, addend 0.
    let mut rela = Vec::new();
    rela.extend_from_slice(&0u64.to_le_bytes());
    rela.extend_from_slice(&((1u64 << 32) | 1).to_le_bytes());
    rela.extend_from_slice(&0i64.to_le_bytes());
    let strtab = b"\0_start\0";
    let shstrtab = b"\0.text\0.rodata\0.rela.rodata\0.symtab\0.strtab\0.shstrtab\0";
    let text_at = 64usize;
    let ro_at = (text_at + text.len()).next_multiple_of(8);
    let rela_at = ro_at + rodata.len();
    let sym_at = rela_at + rela.len();
    let str_at = sym_at + 48;
    let shstr_at = str_at + strtab.len();
    let sh_at = (shstr_at + shstrtab.len()).next_multiple_of(8);
    let mut b = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
    b.resize(16, 0);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&62u16.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&(sh_at as u64).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    for v in [64u16, 0, 0, 64, 7, 6] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(text);
    b.resize(ro_at, 0);
    b.extend_from_slice(&rodata);
    b.extend_from_slice(&rela);
    b.extend_from_slice(&[0; 24]);
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&[0x12, 0]);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&(text.len() as u64).to_le_bytes());
    b.extend_from_slice(strtab);
    b.extend_from_slice(shstrtab);
    b.resize(sh_at, 0);
    let sects: [Shdr; 7] = [
        (0, 0, 0, 0, 0, 0, 0, 0, 0),
        (1, 1, 6, text_at, text.len(), 0, 0, 16, 0),
        (7, 1, 2, ro_at, rodata.len(), 0, 0, 8, 0),
        (15, 4, 0x40, rela_at, rela.len(), 4, 2, 8, 24),
        (28, 2, 0, sym_at, 48, 5, 1, 8, 24),
        (36, 3, 0, str_at, strtab.len(), 0, 0, 1, 0),
        (44, 3, 0, shstr_at, shstrtab.len(), 0, 0, 1, 0),
    ];
    for (name, ty, flags, off, size, link, info, align, entsize) in sects {
        b.extend_from_slice(&name.to_le_bytes());
        b.extend_from_slice(&ty.to_le_bytes());
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&(off as u64).to_le_bytes());
        b.extend_from_slice(&(size as u64).to_le_bytes());
        b.extend_from_slice(&link.to_le_bytes());
        b.extend_from_slice(&info.to_le_bytes());
        b.extend_from_slice(&align.to_le_bytes());
        b.extend_from_slice(&entsize.to_le_bytes());
    }
    b
}

/// A PIE whose read-only data needs a dynamic relocation is refused, as
/// ld.lld refuses it: the loader cannot write there (no `DT_TEXTREL`), so
/// the program would crash at load. A static executable takes it.
#[test]
fn pie_refuses_a_dynamic_relocation_in_read_only_data() {
    let dir = scratch("textrel");
    let o = dir.join("o.o");
    std::fs::write(&o, rodata_pointer_object()).unwrap();
    let args = |kind: &str| {
        let exe = dir.join(kind.trim_start_matches('-'));
        vec![
            "-m".to_owned(),
            "elf_x86_64".to_owned(),
            kind.to_owned(),
            "-o".to_owned(),
            exe.display().to_string(),
            o.display().to_string(),
        ]
    };
    let e = nabild::elf::link(&nabild::elf::parse_args(&args("-pie")).unwrap()).unwrap_err();
    assert!(
        e.contains("relocation R_X86_64_64 in read-only section .rodata")
            && e.contains("recompile with -fPIC"),
        "{e}"
    );
    nabild::elf::link(&nabild::elf::parse_args(&args("-static")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two copies of one function, the first with a local label inside.
const TWINS: &str = "
.section .text.f1,\"ax\",@progbits
.globl f1
f1:
    mov $1, %eax
inner:
    ret
.section .text.f2,\"ax\",@progbits
.globl f2
f2:
    mov $1, %eax
    ret
.section .text._start,\"ax\",@progbits
.globl _start
_start:
    call f1
    call f2
    mov $60, %eax
    xor %edi, %edi
    syscall
";

/// The bytes of section `name` of ELF64 file `b`.
fn section<'a>(b: &'a [u8], name: &str) -> &'a [u8] {
    let (shoff, shnum) = (u64_at(b, 40) as usize, u16_at(b, 60) as usize);
    let sh = |i: usize| shoff + 64 * i;
    let strs = u64_at(b, sh(u16_at(b, 62) as usize) + 24) as usize;
    let at = (0..shnum)
        .map(sh)
        .find(|&h| {
            let n = strs + u32::from_le_bytes(b[h..h + 4].try_into().unwrap()) as usize;
            b[n..].starts_with(name.as_bytes()) && b[n + name.len()] == 0
        })
        .unwrap();
    let (off, size) = (u64_at(b, at + 24) as usize, u64_at(b, at + 32) as usize);
    &b[off..off + size]
}

/// Identical code folding folds the same with `--strip-all` as without:
/// a local symbol inside one copy keeps them apart in both links, so
/// `-O2 -g` stays `-O2`'s code (static musl links, whose C library has
/// such symbols, differed).
#[test]
fn folding_does_not_depend_on_stripping() {
    let dir = scratch("icf-strip");
    let s = dir.join("twins.s");
    std::fs::write(&s, TWINS).unwrap();
    let o = dir.join("twins.o");
    let built = std::process::Command::new("clang")
        .args(["--target=x86_64-linux-gnu", "-c", "-o"])
        .arg(&o)
        .arg(&s)
        .status();
    if !built.is_ok_and(|s| s.success()) {
        eprintln!("no clang: skipped");
        return;
    }
    let text = |strip: bool| {
        let exe = dir.join(if strip { "stripped" } else { "kept" });
        let flags = ["-m", "elf_x86_64", "-static", "--gc-sections", "--icf=safe"];
        let mut args: Vec<std::ffi::OsString> = flags
            .into_iter()
            .chain(strip.then_some("--strip-all"))
            .map(Into::into)
            .collect();
        args.extend([
            "-o".into(),
            exe.clone().into_os_string(),
            o.clone().into_os_string(),
        ]);
        nabild::elf::link(&nabild::elf::parse_args(&args).unwrap()).unwrap();
        section(&std::fs::read(&exe).unwrap(), ".text").to_vec()
    };
    assert_eq!(text(false), text(true));
    let _ = std::fs::remove_dir_all(&dir);
}
