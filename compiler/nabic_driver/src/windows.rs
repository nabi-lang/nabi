//! Windows (`*-pc-windows-msvc`) without MSVC or the Windows SDK: import
//! libraries built in process from the linker's embedded mingw-w64 module
//! definitions (`nabild::coff::defs`), nabi's own CRT startup over UCRT
//! (`nabic_codegen_native::windows_startup`), and lld-link options (nabi's
//! linker takes them for `x86_64`, arm64 and x86). No MSVC static library
//! is linked.

use std::ffi::OsString;
use std::path::Path;

/// `-C subsystem=console|windows`: entry `mainCRTStartup`, or
/// `WinMainCRTStartup` calling `WinMain`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Subsystem {
    #[default]
    Console,
    Windows,
}

impl Subsystem {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "console" => Some(Self::Console),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// lld-link arguments besides objects and archives: startup object and
/// import libraries (written to `dir`), `#[link]` libraries the embedded
/// set lacks, subsystem and entry, and `/debug` with `debug`.
pub fn link_args(
    t: &nabic_target::TargetSpec,
    native_libs: &[(String, bool)],
    subsystem: Subsystem,
    debug: bool,
    dir: &Path,
) -> Result<Vec<OsString>, String> {
    link_args_of(t, native_libs, Some(subsystem), debug, dir)
}

/// [`link_args`] of a DLL: its startup object has no program entry.
pub fn dll_link_args(
    t: &nabic_target::TargetSpec,
    native_libs: &[(String, bool)],
    debug: bool,
    dir: &Path,
) -> Result<Vec<OsString>, String> {
    link_args_of(t, native_libs, None, debug, dir)
}

fn link_args_of(
    t: &nabic_target::TargetSpec,
    native_libs: &[(String, bool)],
    program: Option<Subsystem>,
    debug: bool,
    dir: &Path,
) -> Result<Vec<OsString>, String> {
    let subsystem = program.unwrap_or(Subsystem::Console);
    let defs = nabild::coff::defs::of(t)?;
    let startup = dir.join("nabi_startup.obj");
    let obj = match program {
        Some(s) => nabic_codegen_native::windows_startup(&t.arch, s == Subsystem::Windows)?,
        None => nabic_codegen_native::windows_dll_startup(&t.arch)?,
    };
    std::fs::write(&startup, obj).map_err(|e| format!("{}: {e}", startup.display()))?;
    let mut a = vec![startup.into_os_string()];
    for (name, _) in native_libs {
        if !defs.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
            a.push(format!("{name}.lib").into());
        }
    }
    for (name, def) in defs {
        let lib = dir.join(format!("{name}.lib"));
        nabild::import_library(t, def, &lib)?;
        a.push(lib.into_os_string());
    }
    let (sub, entry) = match subsystem {
        Subsystem::Console => ("console", "mainCRTStartup"),
        Subsystem::Windows => ("windows", "WinMainCRTStartup"),
    };
    a.push(format!("/subsystem:{sub}").into());
    a.push(format!("/entry:{entry}").into());
    // 8 MiB main stack like Linux, macOS and the runtime's own threads
    // (nested `await` runs on it); Windows' default is 1 MiB.
    a.push(format!("/stack:{}", t.stack_size).into());
    // Nothing refers to the TLS directory but the loader. (`/include`
    // takes the symbol, C names `_`-prefixed on x86.)
    a.push(format!("/include:{}_tls_used", t.c_symbol_prefix).into());
    // The PE timestamp and PDB signature: a hash of the output, so equal
    // inputs link to equal files.
    a.push("/Brepro".into());
    if debug {
        a.push("/debug".into());
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(machine, subsystem)` of a PE image.
    fn pe_header(exe: &[u8]) -> (u16, u16) {
        assert_eq!(&exe[..2], b"MZ");
        let pe = u32::from_le_bytes(exe[0x3c..0x40].try_into().unwrap()) as usize;
        assert_eq!(&exe[pe..pe + 4], b"PE\0\0");
        let machine = u16::from_le_bytes([exe[pe + 4], exe[pe + 5]]);
        // The optional header's `Subsystem`.
        let sub = pe + 24 + 68;
        (machine, u16::from_le_bytes([exe[sub], exe[sub + 1]]))
    }

    const MACHINES: [(&str, u16); 3] = [
        ("x86_64-pc-windows-msvc", 0x8664),
        ("aarch64-pc-windows-msvc", 0xaa64),
        ("i686-pc-windows-msvc", 0x14c),
    ];

    /// A COFF object for `machine` of `nabi_main` (x86: `_nabi_main`), the
    /// entry nabi's startup calls, returning 0.
    fn main_object(machine: u16) -> Vec<u8> {
        let code: &[u8] = if machine == 0xaa64 {
            &[0x00, 0x00, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6] // mov w0, #0; ret
        } else {
            &[0x31, 0xc0, 0xc3] // xor eax, eax; ret
        };
        let mut b = Vec::new();
        b.extend_from_slice(&machine.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        let symoff = 20 + 40 + code.len() as u32;
        b.extend_from_slice(&symoff.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(b".text\0\0\0");
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&(code.len() as u32).to_le_bytes());
        b.extend_from_slice(&60u32.to_le_bytes());
        b.extend_from_slice(&[0; 12]);
        b.extend_from_slice(&0x6050_0020u32.to_le_bytes());
        b.extend_from_slice(code);
        // The name is past 8 bytes: in the string table, at 4.
        let name: &[u8] = if machine == 0x14c {
            b"_nabi_main\0"
        } else {
            b"nabi_main\0"
        };
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&0x20u16.to_le_bytes());
        b.extend_from_slice(&[2, 0]);
        b.extend_from_slice(&(4 + name.len() as u32).to_le_bytes());
        b.extend_from_slice(name);
        b
    }

    /// A `nabi_main` alone links into a console / GUI PE with nothing but
    /// what nabic carries: no MSVC, no Windows SDK.
    #[test]
    fn links_a_pe_from_embedded_libraries() {
        let dir = crate::TempDir::new(false).unwrap();
        for (triple, machine) in MACHINES {
            let t = nabic_target::TargetSpec::builtin(triple).unwrap();
            let main = dir.path().join(format!("{triple}.obj"));
            std::fs::write(&main, main_object(machine)).unwrap();
            for (sub, code) in [(Subsystem::Console, 3), (Subsystem::Windows, 2)] {
                let exe = dir.path().join(format!("{triple}-{code}.exe"));
                let libs = [("ws2_32".to_owned(), false)];
                let mut args = vec![main.clone().into_os_string()];
                args.extend(link_args(&t, &libs, sub, false, dir.path()).unwrap());
                nabild::link_executable(&t, &args, &exe, true, None).unwrap();
                assert_eq!(pe_header(&std::fs::read(&exe).unwrap()), (machine, code));
            }
        }
    }

    /// `nabi_main` from a library nabic writes (the COFF archive flavour):
    /// the linker finds it through the archive's symbol table.
    #[test]
    fn links_main_from_an_archive() {
        let dir = crate::TempDir::new(false).unwrap();
        for (triple, machine) in MACHINES {
            let t = nabic_target::TargetSpec::builtin(triple).unwrap();
            let main = dir.path().join(format!("{triple}-main.obj"));
            std::fs::write(&main, main_object(machine)).unwrap();
            let lib = dir.path().join(format!("{triple}-main.lib"));
            nabild::write_archive(&t, &lib, &[&main]).unwrap();
            let exe = dir.path().join(format!("{triple}-lib.exe"));
            let mut args = vec![lib.into_os_string()];
            args.extend(link_args(&t, &[], Subsystem::Console, false, dir.path()).unwrap());
            nabild::link_executable(&t, &args, &exe, true, None).unwrap();
            assert_eq!(pe_header(&std::fs::read(&exe).unwrap()), (machine, 3));
        }
    }
}
