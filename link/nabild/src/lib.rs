//! nabi's own linker: a format-neutral core (archives, symbol
//! resolution, dead stripping, layout), the Mach-O arm64 backend for
//! macOS executables, the ELF backend for `x86_64` / aarch64 / i686 / armv7 /
//! riscv64 Linux ones (glibc and musl), powerpc64le and s390x Linux ones
//! (glibc), `x86_64` FreeBSD ones and riscv32 images without an OS, and
//! the COFF backend for `x86_64` / arm64 / i686 Windows ones and the wasm
//! backend for wasm32 modules (WASI and the browser). Also the `ar` writer
//! for every target's libraries and the Windows import library writer.

#![allow(clippy::many_single_char_names, clippy::cognitive_complexity)]

pub mod archive;
pub mod coff;
pub mod elf;
pub mod gc;
pub mod icf;
pub mod layout;
pub mod macho;
pub mod plugin;
pub mod symtab;
pub mod trace;
pub mod wasm;

mod bytes;

use nabic_target::{Libc, ObjectFormat, TargetSpec};
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Whether this linker links executables for `t`.
#[must_use]
pub fn links(t: &TargetSpec) -> bool {
    match t.object_format {
        ObjectFormat::MachO => {
            t.arch == "aarch64" && nabic_target::apple::Platform::of(t).is_some()
        }
        ObjectFormat::Elf => match t.os.as_str() {
            "linux" => match t.arch.as_str() {
                "x86_64" | "aarch64" | "x86" | "arm" | "riscv64" => {
                    matches!(t.libc, Libc::Glibc | Libc::Musl)
                }
                "powerpc64" => t.endian == "little" && t.libc == Libc::Glibc,
                "s390x" => t.libc == Libc::Glibc,
                _ => false,
            },
            "freebsd" => t.arch == "x86_64" && t.libc == Libc::FreeBsd,
            "android" => matches!(t.arch.as_str(), "x86_64" | "aarch64") && t.libc == Libc::Bionic,
            "none" => t.arch == "riscv32" && t.libc == Libc::None,
            _ => false,
        },
        ObjectFormat::Coff => {
            matches!(t.arch.as_str(), "x86_64" | "aarch64" | "x86") && t.os == "windows"
        }
        ObjectFormat::Wasm => t.arch == "wasm32",
    }
}

/// Link executable `out` for `t` against the platform C library.
///
/// `inputs`: objects, archives, `-l`, `-L` and linker options (ld64's
/// for Mach-O, ld.lld's for ELF, lld-link's for COFF); `strip` drops
/// unreferenced code; `LIBRARY_PATH` directories follow `-L` ones.
/// Mach-O: the SDK comes from `SDKROOT`, else the developer directory.
/// ELF: `libc` is the toolchain's C runtime and library for `t`
/// (`targets/build.sh`). COFF: no default libraries; `inputs` bring the
/// CRT (via [`import_library`]) and the entry point. Wasm: wasm-ld's
/// options; `libc` is wasi-libc's directory (its `crt1-*.o` and
/// `libc.a`); unreferenced code is always dropped.
pub fn link_executable<S: AsRef<OsStr>>(
    t: &TargetSpec,
    inputs: &[S],
    out: &Path,
    strip: bool,
    libc: Option<&Path>,
) -> Result<String, String> {
    link_image(t, inputs, out, strip, libc, None, None)
}

/// What a shared library adds to a link.
pub struct Shared<'a> {
    /// The C names it exports (no platform prefix).
    pub exports: &'a [String],
    /// The function the loader runs when it loads the library.
    pub init: &'a str,
    /// `(word, symbol)`: the linker writes the symbol's address less the
    /// word's own into the pointer-sized word.
    pub anchors: &'a [(String, String)],
    /// Windows: where the import library goes.
    pub implib: Option<&'a Path>,
    /// ELF: `DT_SONAME`; the file name when `None`.
    pub soname: Option<&'a str>,
    /// Mach-O: `(current, compatibility)` versions, `x.y.z`; 1.0.0 when
    /// `None`.
    pub versions: Option<(&'a str, &'a str)>,
}

/// Whether this linker links shared libraries for `t`.
#[must_use]
pub fn links_shared(t: &TargetSpec) -> bool {
    links(t)
        && match t.object_format {
            ObjectFormat::MachO => true,
            ObjectFormat::Elf => {
                matches!(
                    (t.os.as_str(), t.libc),
                    ("linux", Libc::Glibc) | ("android", Libc::Bionic)
                ) && matches!(t.arch.as_str(), "x86_64" | "aarch64")
            }
            ObjectFormat::Coff => t.arch == "x86_64" || t.arch == "aarch64",
            ObjectFormat::Wasm => false,
        }
}

/// Link shared library `out` (`.so`, `.dylib`, `.dll`) for `t`: as
/// [`link_executable`], with no entry point; only `sh.exports` are
/// exported, and `sh.init` runs at load.
pub fn link_shared<S: AsRef<OsStr>>(
    t: &TargetSpec,
    inputs: &[S],
    out: &Path,
    libc: Option<&Path>,
    sh: &Shared<'_>,
) -> Result<String, String> {
    if !links_shared(t) {
        return Err(format!(
            "Nabi's linker does not link shared libraries for {} yet",
            t.name
        ));
    }
    link_image(t, inputs, out, true, libc, Some(sh), None)
}

/// Whether this linker links plugins and plugin hosts for `t`.
#[must_use]
pub fn links_plugin(t: &TargetSpec) -> bool {
    // An iOS device loads only code signed into the app.
    links_shared(t)
        && nabic_target::apple::Platform::of(t) != Some(nabic_target::apple::Platform::Ios)
}

/// Link plugin `out` (`.dylib`, `.so`, `.dll`) for `t`.
///
/// A shared library that exports only its `nabi_module` and takes every
/// Nabi symbol of the process through a slot. `sh.exports` and `sh.init` are set here; the
/// glue object `glue` builds holds the stubs, slots and import table.
pub fn link_plugin<S: AsRef<OsStr>>(
    t: &TargetSpec,
    inputs: &[S],
    out: &Path,
    libc: Option<&Path>,
    sh: &Shared<'_>,
    p: &plugin::Plugin<'_>,
    glue: &plugin::Glue<'_>,
) -> Result<String, String> {
    if !links_plugin(t) {
        return Err(format!(
            "Nabi's linker does not link plugins for {} yet",
            t.name
        ));
    }
    let exports = [plugin::MODULE.to_owned()];
    let init = if t.object_format == ObjectFormat::Coff {
        plugin::DLL_ENTRY
    } else {
        ""
    };
    let sh = Shared {
        exports: &exports,
        init,
        ..*sh
    };
    link_image(t, inputs, out, true, libc, Some(&sh), Some((p, glue)))
}

/// Link host executable `out` for `t`: as [`link_executable`], keeping
/// every runtime entry and listing the program's Nabi symbols in the
/// export table plugins bind to.
pub fn link_host<S: AsRef<OsStr>>(
    t: &TargetSpec,
    inputs: &[S],
    out: &Path,
    strip: bool,
    libc: Option<&Path>,
    p: &plugin::Plugin<'_>,
    glue: &plugin::Glue<'_>,
) -> Result<String, String> {
    if !links_plugin(t) {
        return Err(format!(
            "Nabi's linker does not link plugin hosts for {} yet",
            t.name
        ));
    }
    link_image(t, inputs, out, strip, libc, None, Some((p, glue)))
}

type PluginLink<'a> = Option<(&'a plugin::Plugin<'a>, &'a plugin::Glue<'a>)>;

fn link_image<S: AsRef<OsStr>>(
    t: &TargetSpec,
    inputs: &[S],
    out: &Path,
    strip: bool,
    libc: Option<&Path>,
    sh: Option<&Shared<'_>>,
    pl: PluginLink<'_>,
) -> Result<String, String> {
    if !links(t) {
        return Err(format!("Nabi's linker does not link {} yet", t.name));
    }
    match t.object_format {
        ObjectFormat::Elf => return link_elf(t, inputs, out, strip, libc, sh, pl),
        ObjectFormat::Coff => return link_coff(t, inputs, out, strip, sh, pl),
        ObjectFormat::Wasm => return link_wasm(t, inputs, out, strip, libc),
        ObjectFormat::MachO => {}
    }
    let search = library_path();
    let (dirs, rest): (Vec<OsString>, Vec<OsString>) = inputs
        .iter()
        .map(|a| a.as_ref().to_owned())
        .partition(|a| a.to_str().is_some_and(|s| s.starts_with("-L")));
    let platform = nabic_target::apple::Platform::of(t)
        .ok_or_else(|| format!("Nabi's linker does not link {} yet", t.name))?;
    let sdk = nabic_target::apple::sdk_of(platform)?;
    let min = &t.deployment_target;
    let sdk_version = nabic_target::apple::sdk_version(&sdk).unwrap_or_else(|| min.clone());
    let mut a: Vec<OsString> = t.pre_link_args.iter().map(OsString::from).collect();
    a.extend(["-platform_version", platform.name(), min, &sdk_version].map(OsString::from));
    a.extend(["-syslibroot".into(), sdk.into_os_string()]);
    // ld64 also searches the real /usr/local/lib.
    if Path::new("/usr/local/lib").is_dir() {
        a.push("-L/usr/local/lib".into());
    }
    a.extend(["-o".into(), out.as_os_str().to_owned()]);
    a.extend(dirs);
    a.extend(search);
    a.extend(rest);
    if strip {
        a.push("-dead_strip".into());
    }
    a.extend(t.late_link_args.iter().map(OsString::from));
    if let Some(sh) = sh {
        let name = out.file_name().unwrap_or_default().to_string_lossy();
        a.extend(["-dylib".into(), "-install_name".into()]);
        a.push(format!("@rpath/{name}").into());
        if let Some((cur, compat)) = sh.versions {
            a.extend(
                ["-current_version", cur, "-compatibility_version", compat].map(OsString::from),
            );
        }
        for e in sh.exports {
            a.extend(["-exported_symbol".into(), format!("_{e}").into()]);
        }
        if !sh.init.is_empty() {
            a.extend(["-init".into(), format!("_{}", sh.init).into()]);
        }
        for (w, s) in sh.anchors {
            a.extend(["-nabi_anchor".into(), format!("_{w}=_{s}").into()]);
        }
    }
    let o = macho::parse_args(&a)?;
    macho::link_with(&o, pl)?;
    Ok(String::new())
}

/// `-L<dir>` of each `LIBRARY_PATH` directory.
fn library_path() -> Vec<OsString> {
    let search = std::env::var_os("LIBRARY_PATH").unwrap_or_default();
    std::env::split_paths(&search)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| {
            let mut l = OsString::from("-L");
            l.push(d);
            l
        })
        .collect()
}

/// An ELF link as ld.lld's command line: the target's arguments, its C
/// runtime objects and library from `libc` (none without a C library),
/// `inputs` after `-L` directories.
fn link_elf<S: AsRef<OsStr>>(
    t: &TargetSpec,
    inputs: &[S],
    out: &Path,
    strip: bool,
    libc: Option<&Path>,
    sh: Option<&Shared<'_>>,
    pl: PluginLink<'_>,
) -> Result<String, String> {
    let dir = match libc {
        Some(d) => d,
        None if t.libc == Libc::None => Path::new(""),
        None => return Err(format!("no C library for {}", t.name)),
    };
    if t.crt_objects.iter().any(|o| !dir.join(o).is_file()) {
        return Err(format!("no C library for {} in {}", t.name, dir.display()));
    }
    let (dirs, rest): (Vec<OsString>, Vec<OsString>) = inputs
        .iter()
        .map(|a| a.as_ref().to_owned())
        .partition(|a| a.to_str().is_some_and(|s| s.starts_with("-L")));
    let mut a: Vec<OsString> = t.pre_link_args.iter().map(OsString::from).collect();
    // Debug info of `--gc-sections`-dropped code points at the DWARF
    // tombstone (all ones), not 0, the start of the address space.
    a.extend(
        [
            "--eh-frame-hdr",
            "--build-id",
            "-z",
            "dead-reloc-in-nonalloc=.debug_info=0xffffffffffffffff",
        ]
        .map(OsString::from),
    );
    if let Some(sh) = sh {
        let name = out.file_name().unwrap_or_default().to_string_lossy();
        a.extend(["-shared", "--hash-style=gnu", "-soname"].map(OsString::from));
        a.push(sh.soname.unwrap_or(&name).into());
        a.extend(
            sh.exports
                .iter()
                .map(|e| format!("--nabi-export={e}").into()),
        );
        if !sh.init.is_empty() {
            a.extend(["-init".into(), sh.init.into()]);
        }
        // bionic refuses initial-exec TLS in a `dlopen`ed library.
        if t.libc == Libc::Bionic {
            a.push("--nabi-dynamic-tls".into());
        }
        a.extend(
            sh.anchors
                .iter()
                .map(|(w, s)| format!("--nabi-anchor={w}={s}").into()),
        );
    } else if t.dynamic_linking {
        a.extend(["-pie", "--hash-style=gnu", "-dynamic-linker"].map(OsString::from));
        a.push((&t.dynamic_loader).into());
    } else {
        a.push("-static".into());
    }
    a.extend(["-o".into(), out.as_os_str().to_owned()]);
    // A shared library has no program entry (`crt1.o`, `Scrt1.o`);
    // bionic's has crt objects of its own.
    a.extend(
        t.crt_objects
            .iter()
            .filter(|o| sh.is_none() || !o.contains("crt1"))
            .map(|o| dir.join(shared_crt(o, sh.is_some())).into_os_string()),
    );
    if !dir.as_os_str().is_empty() {
        let mut l = OsString::from("-L");
        l.push(dir);
        a.push(l);
    }
    a.extend(dirs);
    a.extend(library_path());
    // bionic keeps libpthread and librt in libc, and the NDK has no
    // archives of them.
    a.extend(
        rest.into_iter()
            .filter(|x| !(t.libc == Libc::Bionic && (x == "-lpthread" || x == "-lrt"))),
    );
    if strip {
        a.push("--gc-sections".into());
    }
    // A file of the target's directory, or an argument.
    a.extend(t.late_link_args.iter().map(|x| -> OsString {
        if x.starts_with('-') {
            x.into()
        } else {
            dir.join(shared_crt(x, sh.is_some())).into_os_string()
        }
    }));
    let o = elf::parse_args(&a)?;
    elf::link_with(&o, pl)?;
    Ok(String::new())
}

/// The crt object a shared library takes in place of executable one
/// `o` (bionic's).
fn shared_crt(o: &str, shared: bool) -> &str {
    match o {
        "crtbegin_dynamic.o" if shared => "crtbegin_so.o",
        "crtend_android.o" if shared => "crtend_so.o",
        _ => o,
    }
}

/// A wasm link as wasm-ld's command line: the stack first (an overflow
/// wraps past 0 to memory never grown into, so it traps), the target's
/// arguments, wasi-libc's start file, `inputs` after `-L` directories,
/// then the C library.
fn link_wasm<S: AsRef<OsStr>>(
    t: &TargetSpec,
    inputs: &[S],
    out: &Path,
    strip: bool,
    libc: Option<&Path>,
) -> Result<String, String> {
    let dir = match libc {
        Some(d) => d,
        None if !t.libc.shipped() => Path::new(""),
        None => return Err(format!("no C library for {}", t.name)),
    };
    if t.crt_objects.iter().any(|o| !dir.join(o).is_file()) {
        return Err(format!("no C library for {} in {}", t.name, dir.display()));
    }
    let (dirs, rest): (Vec<OsString>, Vec<OsString>) = inputs
        .iter()
        .map(|a| a.as_ref().to_owned())
        .partition(|a| a.to_str().is_some_and(|s| s.starts_with("-L")));
    let mut a: Vec<OsString> = ["--stack-first", "-z"].map(OsString::from).to_vec();
    a.push(format!("stack-size={}", t.stack_size).into());
    a.push("--max-memory=4294901760".into());
    a.extend(t.pre_link_args.iter().map(OsString::from));
    a.extend(["-o".into(), out.as_os_str().to_owned()]);
    a.extend(t.crt_objects.iter().map(|o| dir.join(o).into_os_string()));
    if !dir.as_os_str().is_empty() {
        let mut l = OsString::from("-L");
        l.push(dir);
        a.push(l);
    }
    a.extend(dirs);
    a.extend(library_path());
    a.extend(rest);
    if strip {
        a.push("--gc-sections".into());
    }
    // A file of the target's directory, or an argument.
    a.extend(t.late_link_args.iter().map(|x| -> OsString {
        if x.starts_with('-') {
            x.into()
        } else {
            dir.join(x).into_os_string()
        }
    }));
    let o = wasm::parse_args(&a)?;
    wasm::link(&o)
}

/// A COFF link as lld-link's command line: no default libraries, a
/// relocatable image with a non-executable stack, `-L` directories (and
/// `LIBRARY_PATH`'s) as `/libpath`.
fn link_coff<S: AsRef<OsStr>>(
    t: &TargetSpec,
    inputs: &[S],
    out: &Path,
    strip: bool,
    sh: Option<&Shared<'_>>,
    pl: PluginLink<'_>,
) -> Result<String, String> {
    let (dirs, rest): (Vec<OsString>, Vec<OsString>) = inputs
        .iter()
        .map(|a| a.as_ref().to_owned())
        .partition(|a| a.to_str().is_some_and(|s| s.starts_with("-L")));
    let mut a: Vec<OsString> = ["/nologo", "/nodefaultlib", "/dynamicbase", "/nxcompat"]
        .map(OsString::from)
        .to_vec();
    a.extend(t.pre_link_args.iter().map(OsString::from));
    let mut o = OsString::from("/out:");
    o.push(out);
    a.push(o);
    for d in dirs.into_iter().chain(library_path()) {
        let mut l = OsString::from("/libpath:");
        l.push(&d.to_string_lossy()[2..]);
        a.push(l);
    }
    a.extend(rest);
    a.push(if strip { "/opt:ref" } else { "/opt:noref" }.into());
    a.extend(t.late_link_args.iter().map(OsString::from));
    if let Some(sh) = sh {
        let m = |n: &str| match t.arch.as_str() {
            "x86" => format!("_{n}"),
            _ => n.to_owned(),
        };
        a.push("/dll".into());
        a.push(format!("/entry:{}", sh.init).into());
        a.extend(sh.exports.iter().map(|e| format!("/export:{e}").into()));
        a.extend(
            sh.anchors
                .iter()
                .map(|(w, s)| format!("/nabianchor:{}={}", m(w), m(s)).into()),
        );
        if let Some(p) = sh.implib {
            let mut l = OsString::from("/implib:");
            l.push(p);
            a.push(l);
        }
    }
    let o = coff::parse_args(&a)?;
    coff::link_with(&o, pl)?;
    Ok(String::new())
}

/// The import library `out` of the module definition `def` (a `.def`
/// file's text, mingw-w64 syntax) for a Windows target; x86 names are
/// decorated and imported undecorated (`llvm-dlltool -k`).
pub fn import_library(t: &TargetSpec, def: &str, out: &Path) -> Result<(), String> {
    let machine = t
        .coff_machine()
        .ok_or_else(|| format!("{} has no COFF machine type", t.name))?;
    let bytes = coff::import::import_library(machine, def)?;
    let _ = std::fs::remove_file(out);
    std::fs::write(out, bytes).map_err(|e| format!("cannot write {}: {e}", out.display()))
}

/// An archive of `members` with a symbol table: BSD layout for Apple
/// targets, GNU with the second linker member for Windows, else GNU.
pub fn write_archive(t: &TargetSpec, out: &Path, members: &[&Path]) -> Result<(), String> {
    let kind = match t.object_format {
        ObjectFormat::MachO => archive::Kind::Darwin,
        ObjectFormat::Coff => archive::Kind::Coff,
        _ => archive::Kind::Gnu,
    };
    let mut m = Vec::with_capacity(members.len());
    for p in members {
        let data = std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        let name = p
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        m.push((name, data));
    }
    let bytes = archive::write(kind, &m);
    let _ = std::fs::remove_file(out);
    std::fs::write(out, bytes).map_err(|e| format!("cannot write {}: {e}", out.display()))
}

/// One archive `out` of the members of archives and objects `inputs`.
///
/// In order: a static library that holds all a C program links. Members
/// are renamed `<input>-<name>`, so like-named ones of two archives both
/// stay.
pub fn merge_archives(t: &TargetSpec, out: &Path, inputs: &[&Path]) -> Result<(), String> {
    let kind = match t.object_format {
        ObjectFormat::MachO => archive::Kind::Darwin,
        ObjectFormat::Coff => archive::Kind::Coff,
        _ => archive::Kind::Gnu,
    };
    let mut m = Vec::new();
    for (i, p) in inputs.iter().enumerate() {
        let data = std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        if data.starts_with(archive::MAGIC) {
            let a = archive::parse(&data).map_err(|e| format!("{}: {e}", p.display()))?;
            for mem in a.members {
                if mem.name.starts_with("__.SYMDEF") || mem.name.starts_with('/') {
                    continue;
                }
                m.push((format!("{i}-{}", mem.name), mem.data.to_vec()));
            }
        } else {
            let name = p
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            m.push((format!("{i}-{name}"), data));
        }
    }
    let bytes = archive::write(kind, &m);
    let _ = std::fs::remove_file(out);
    std::fs::write(out, bytes).map_err(|e| format!("cannot write {}: {e}", out.display()))
}

/// Read `data` as a link reads an input (an object, archive, shared
/// object or import library of any format, by its magic), keeping
/// nothing: what fuzzing drives. `Err` for input the readers reject.
pub fn read_input(data: &[u8]) -> Result<(), String> {
    read_input_at(data, 0)
}

fn read_input_at(data: &[u8], depth: u32) -> Result<(), String> {
    let name = "fuzz.o".to_owned();
    if data.starts_with(archive::MAGIC) {
        let a = archive::parse(data)?;
        if depth == 0 {
            for m in &a.members {
                let _ = read_input_at(m.data, 1);
            }
        }
        return Ok(());
    }
    if data.starts_with(b"\x7fELF") {
        let wide = data.get(4) == Some(&2);
        let machine = data
            .get(18..20)
            .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        let big = data.get(5) == Some(&2);
        let machine = if big { machine.swap_bytes() } else { machine };
        let arch = match (machine, wide) {
            (62, _) => elf::Arch::X86_64,
            (183, _) => elf::Arch::Aarch64,
            (3, _) => elf::Arch::I386,
            (40, _) => elf::Arch::Arm,
            (243, true) => elf::Arch::Riscv64,
            (243, false) => elf::Arch::Riscv32,
            (21, _) => elf::Arch::Ppc64,
            (22, _) => elf::Arch::S390x,
            _ => return Err("unknown ELF machine".to_owned()),
        };
        if data.get(16) == Some(&3) {
            return elf::dso::read(Path::new("fuzz.so"), data, arch).map(|_| ());
        }
        let mut l = elf::Link::new(arch, true);
        return elf::object::parse(&mut l, 0, name, data.to_vec()).map(|_| ());
    }
    if data.starts_with(&0xfeed_facf_u32.to_le_bytes()) {
        let cpu = data
            .get(4..8)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let arch = if cpu == 0x0100_0007 {
            "x86_64"
        } else {
            "arm64"
        };
        let mut l = macho::Link::new(arch).ok_or("unknown Mach-O architecture")?;
        return macho::object::parse(&mut l, 0, name.clone(), name, data.to_vec()).map(|_| ());
    }
    if data.starts_with(b"\0asm") {
        let mut l = wasm::Link::new();
        return wasm::object::parse(&mut l, 0, name, data.to_vec(), true);
    }
    let machine = data
        .get(..2)
        .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    let arch = match machine {
        coff::MACHINE_ARM64 => coff::Arch::Aarch64,
        coff::MACHINE_I386 => coff::Arch::X86,
        _ => coff::Arch::X86_64,
    };
    let mut l = coff::Link::new(arch);
    if coff::import::is_short_import(data) {
        return coff::import::load(&mut l, &name, data);
    }
    let fi = l.files.len() as u32;
    coff::object::parse(&mut l, fi, name, String::new(), data.to_vec()).map(|_| ())
}
