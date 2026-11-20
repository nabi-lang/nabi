//! Where a target's system headers are, as clang's driver finds them, and
//! the compiler options bindgen takes after `--`.

use crate::{Dir, DirKind};
use nabicc_target::{Arch, Os, Target};
use std::path::{Path, PathBuf};

/// The options clang would take that change what a header declares.
#[derive(Default)]
pub struct Opts {
    pub quote: Vec<PathBuf>,
    pub user: Vec<PathBuf>,
    pub system: Vec<PathBuf>,
    pub after: Vec<PathBuf>,
    /// `-D` / `-U` in order, as `#define` / `#undef` lines.
    pub defines: String,
    pub include: Vec<PathBuf>,
    pub sysroot: Option<PathBuf>,
    pub no_std_inc: bool,
    pub no_stdlib_inc: bool,
    pub no_builtin_inc: bool,
}

const HELP: &str = "bindgen takes -I <dir>, -isystem <dir>, -iquote <dir>, -idirafter <dir>, \
                    -D <name>[=<value>], -U <name>, -include <file>, --sysroot <dir>, \
                    -isysroot <dir>, -nostdinc, -nostdlibinc, -nobuiltininc and -std=<c dialect> \
                    after `--`";

impl Opts {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut o = Self::default();
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            i += 1;
            // `-Ifoo` and `-I foo` alike.
            let mut val = |flag: &str| -> Result<Option<String>, String> {
                let Some(rest) = a.strip_prefix(flag) else {
                    return Ok(None);
                };
                let rest = rest.strip_prefix('=').unwrap_or(rest);
                if !rest.is_empty() {
                    return Ok(Some(rest.to_owned()));
                }
                let v = args
                    .get(i)
                    .ok_or_else(|| format!("`{flag}` needs a value after it"))?;
                i += 1;
                Ok(Some(v.clone()))
            };
            if let Some(v) = val("-isystem")? {
                o.system.push(v.into());
            } else if let Some(v) = val("-iquote")? {
                o.quote.push(v.into());
            } else if let Some(v) = val("-idirafter")? {
                o.after.push(v.into());
            } else if let Some(v) = val("-isysroot")? {
                o.sysroot = Some(v.into());
            } else if let Some(v) = val("--sysroot")? {
                o.sysroot = Some(v.into());
            } else if a == "-include" {
                let v = args.get(i).ok_or("`-include` needs a file after it")?;
                i += 1;
                o.include.push(v.into());
            } else if a == "-nostdinc" {
                o.no_std_inc = true;
            } else if a == "-nostdlibinc" {
                o.no_stdlib_inc = true;
            } else if a == "-nobuiltininc" {
                o.no_builtin_inc = true;
            } else if let Some(v) = val("-I")? {
                o.user.push(v.into());
            } else if let Some(v) = val("-D")? {
                let (n, d) = v.split_once('=').unwrap_or((&v, "1"));
                o.defines.push_str(&format!("#define {n} {d}\n"));
            } else if let Some(v) = val("-U")? {
                o.defines.push_str(&format!("#undef {v}\n"));
            } else if a.starts_with("-std=") || a == "-w" || a.starts_with("-W") {
                // The dialect is C17 with GNU extensions; warnings are not
                // reported.
            } else {
                return Err(format!("unknown option `{a}`: {HELP}"));
            }
        }
        Ok(o)
    }
}

/// The include search list: the options' directories, then the target's
/// system ones (`host`: this machine's own serve the target; `sdk`: the
/// Apple SDK), around nabic's built-in headers.
pub fn dirs(t: &Target, o: &Opts, host: bool, sdk: Option<&Path>) -> Vec<Dir> {
    let d = |p: &Path, system: bool, kind: DirKind| Dir {
        path: p.to_path_buf(),
        system,
        kind,
    };
    let mut out: Vec<Dir> = Vec::new();
    out.extend(o.quote.iter().map(|p| d(p, false, DirKind::Quote)));
    out.extend(o.user.iter().map(|p| d(p, false, DirKind::Normal)));
    out.extend(o.system.iter().map(|p| d(p, true, DirKind::Normal)));
    if !o.no_std_inc {
        let builtin = (!o.no_builtin_inc).then(|| Dir {
            path: PathBuf::from(crate::BUILTIN_DIR),
            system: true,
            kind: DirKind::Builtin,
        });
        let root = match (&o.sysroot, sdk) {
            (Some(r), _) => Some(r.clone()),
            (None, Some(s)) => Some(s.to_path_buf()),
            (None, None) if host && t.os != Os::Windows => Some(PathBuf::from("/")),
            (None, None) => None,
        };
        let root = root.filter(|_| !o.no_stdlib_inc);
        let sys = |p: PathBuf| p.is_dir().then(|| d(&p, true, DirKind::Normal));
        match (t.os, root) {
            (Os::Windows, _) => {
                out.extend(builtin);
                if host && !o.no_stdlib_inc {
                    out.extend(
                        msvc_include_dirs()
                            .into_iter()
                            .map(|p| d(&p, true, DirKind::Normal)),
                    );
                }
            }
            (Os::Darwin, Some(r)) => {
                out.extend(sys(r.join("usr/local/include")));
                out.extend(builtin);
                out.extend(sys(r.join("usr/include")));
                let fw = r.join("System/Library/Frameworks");
                if fw.is_dir() {
                    out.push(d(&fw, true, DirKind::Framework));
                }
            }
            (Os::Wasi | Os::None, Some(r)) if t.arch == Arch::Wasm32 => {
                out.extend(builtin);
                let triple = t.triple.as_str();
                out.extend(sys(r.join("include").join(triple)));
                out.extend(sys(r.join("include")));
            }
            (_, Some(r)) => {
                out.extend(sys(r.join("usr/local/include")));
                out.extend(builtin);
                if t.os == Os::Linux {
                    for m in multiarch(t) {
                        out.extend(sys(r.join("usr/include").join(m)));
                    }
                }
                out.extend(sys(r.join("usr/include")));
            }
            (_, None) => out.extend(builtin),
        }
    }
    out.extend(o.after.iter().map(|p| d(p, true, DirKind::Normal)));
    out
}

/// Debian's multiarch directory names for a Linux target (the NDK's on
/// Android).
fn multiarch(t: &Target) -> Vec<&'static str> {
    let musl = t.musl;
    if t.android != 0 {
        return match t.arch {
            Arch::X86_64 => vec!["x86_64-linux-android"],
            Arch::Aarch64 => vec!["aarch64-linux-android"],
            _ => vec![],
        };
    }
    match t.arch {
        Arch::X86_64 if musl => vec!["x86_64-linux-musl"],
        Arch::X86_64 => vec!["x86_64-linux-gnu"],
        Arch::X86 => vec!["i386-linux-gnu", "i686-linux-gnu"],
        Arch::Aarch64 if musl => vec!["aarch64-linux-musl"],
        Arch::Aarch64 => vec!["aarch64-linux-gnu"],
        Arch::Arm if musl => vec!["arm-linux-musleabihf"],
        Arch::Arm => vec!["arm-linux-gnueabihf"],
        Arch::Riscv64 => vec!["riscv64-linux-gnu"],
        Arch::Riscv32 => vec!["riscv32-linux-gnu"],
        Arch::Ppc64le => vec!["powerpc64le-linux-gnu"],
        Arch::S390x => vec!["s390x-linux-gnu"],
        Arch::Wasm32 => vec![],
    }
}

/// MSVC's and the Windows SDK's include directories: `INCLUDE` (a
/// developer prompt's), else the newest Visual Studio (`vswhere`) and
/// Windows 10/11 SDK in their default places.
pub fn msvc_include_dirs() -> Vec<PathBuf> {
    if let Some(inc) = std::env::var_os("INCLUDE").filter(|v| !v.is_empty()) {
        return std::env::split_paths(&inc).filter(|p| p.is_dir()).collect();
    }
    let mut out = Vec::new();
    let pf86 = std::env::var_os("ProgramFiles(x86)")
        .map_or_else(|| PathBuf::from(r"C:\Program Files (x86)"), PathBuf::from);
    // Visual Studio's C runtime headers (vcruntime.h).
    let vswhere = pf86.join(r"Microsoft Visual Studio\Installer\vswhere.exe");
    let installs: Vec<PathBuf> = std::process::Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| PathBuf::from(l.trim()))
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    for vs in installs {
        if let Some(v) = newest_dir(&vs.join(r"VC\Tools\MSVC")) {
            out.push(v.join("include"));
            break;
        }
    }
    // The Windows SDK: ucrt, um, shared, winrt.
    let kits = std::env::var_os("WindowsSdkDir")
        .map_or_else(|| pf86.join(r"Windows Kits\10"), PathBuf::from);
    if let Some(v) = newest_dir(&kits.join("Include")) {
        for sub in ["ucrt", "um", "shared", "winrt", "cppwinrt"] {
            let p = v.join(sub);
            if p.is_dir() {
                out.push(p);
            }
        }
    }
    out
}

/// The subdirectory with the highest dotted version name.
fn newest_dir(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(v) = name
            .split('.')
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
        else {
            continue;
        };
        if e.path().is_dir() && best.as_ref().is_none_or(|b| v > b.0) {
            best = Some((v, e.path()));
        }
    }
    best.map(|b| b.1)
}
