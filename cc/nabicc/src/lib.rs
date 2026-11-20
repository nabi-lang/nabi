//! Nabi's C compiler, `nabicc` (`nabi cc`): C89 to C23 with the GNU,
//! Clang, Apple and Microsoft extensions system headers use, read as
//! clang reads it for the target (the same predefined macros, include
//! search and type layout), compiled by Nabi's own backend and linked by
//! its own linker. No other C compiler takes part.
//!
//! The same front end serves `nabi bindgen` (`parse`).

// Tests name values as C does (`a`, `b`, `l`, `r`).
#![allow(clippy::many_single_char_names)]

pub mod flags;
pub mod headers;
pub mod link;
pub mod unit;

#[cfg(test)]
mod tests;

pub use nabicc_lex as lex;
pub use nabicc_parse as parse;
pub use nabicc_parse::read::{Config, header as parse};
pub use nabicc_parse::ty;
pub use nabicc_pp as pp;
pub use nabicc_pp::{Error, builtin, sysinc};
pub use nabicc_target as target;

use crate::flags::{Flags, Mode};
use nabic_target::TargetSpec;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The version, and the commit and its date when built from git.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("NABI_COMMIT_INFO"));

const USAGE: &str = "\
Nabi's C compiler

Usage: nabicc [options] <file>...
       nabi cc [options] <file>...

Options:
  -c                 compile to objects, do not link
  -S                 compile to assembly
  -E                 preprocess only
  -o <file>          the output
  -I <dir>           search <dir> for headers (-isystem, -iquote, -idirafter too)
  -D <name>[=<val>]  define a macro; -U <name> removes one
  -include <file>    read <file> first
  -std=<std>         c89, c99, c11, c17, c23 or gnu89 .. gnu23 (default gnu17)
  -O<level>          0, 1, 2, 3, s or z
  -g                 debug info
  -l<lib>, -L<dir>   link with a library, search a directory
  -Wl,<opts>         pass options to the linker
  -M, -MM, -MD, -MMD, -MF <file>, -MT <target>, -MP
                     dependency lists for make
  --target=<triple>  compile for another target (`nabi --print target-list`)
  --version          print the version

Warnings (-W*), code generation options (-f*, -m*) and -fPIC are accepted
and change nothing: the code is always position independent.
";

/// The target `--target` names: Nabi's names, and the triples clang and
/// gcc take for them.
pub fn resolve_target(name: Option<&str>) -> Result<TargetSpec, String> {
    let Some(n) = name else {
        return TargetSpec::resolve(None);
    };
    // An in-progress target says so, not that the name is unknown.
    let found = |c: &str| match TargetSpec::resolve(Some(c)) {
        Ok(t) => Some(Ok(t)),
        Err(e) if nabic_target::in_progress(c) => Some(Err(e)),
        Err(_) => None,
    };
    if let Some(r) = found(n) {
        return r;
    }
    let parts: Vec<&str> = n.split('-').collect();
    let arch = match parts.first().copied() {
        Some("arm64") => "aarch64",
        Some("i386" | "i486" | "i586") => "i686",
        Some("armv7" | "armv7l" | "armv7a" | "arm") => "armv7",
        Some("riscv64") => "riscv64gc",
        Some("ppc64le") => "powerpc64le",
        Some(a) => a,
        None => n,
    };
    let rest = parts.get(1..).unwrap_or(&[]).join("-");
    let cands = [
        format!("{arch}-{rest}"),
        format!("{arch}-unknown-{rest}"),
        format!("{arch}-unknown-{}", rest.replace("pc-", "")),
        format!("{arch}-apple-darwin"),
    ];
    for c in &cands {
        let darwin = n.contains("apple") || n.contains("darwin") || n.contains("macos");
        if c.ends_with("apple-darwin") && !darwin {
            continue;
        }
        if let Some(r) = found(c) {
            return r;
        }
    }
    // `arm-linux-gnueabi`, `armv7l-unknown-linux-musl`, ...: the armv7
    // target of that C library.
    if arch == "armv7" {
        let libc = if rest.contains("musl") {
            "musleabihf"
        } else {
            "gnueabihf"
        };
        if let Some(r) = found(&format!("armv7-unknown-linux-{libc}")) {
            return r;
        }
    }
    TargetSpec::resolve(Some(n))
}

/// The object of C's `main` called by the Windows startup's `nabi_main`,
/// and `atexit` (MSVC's static library has it, UCRT `_crt_atexit`).
pub fn main_shim(t: &TargetSpec) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join(format!("nabicc-shim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let src = dir.join("main.c");
    std::fs::write(
        &src,
        "extern int main(int, char **, char **);\n\
         int *__p___argc(void);\nchar ***__p___argv(void);\nchar ***__p__environ(void);\n\
         int nabi_main(void) { return main(*__p___argc(), *__p___argv(), *__p__environ()); }\n\
         int _crt_atexit(void (*)(void));\n\
         int atexit(void (*f)(void)) { return _crt_atexit(f); }\n",
    )
    .map_err(|e| format!("{}: {e}", src.display()))?;
    let f = Flags {
        mode: Mode::Compile,
        pp: vec!["-nostdinc".to_owned()],
        opt: "2".to_owned(),
        ..Flags::default()
    };
    let o = unit::compile(&src, &f, t);
    let _ = std::fs::remove_dir_all(&dir);
    o.map(|o| o.bytes)
}

/// How the user called it: `nabi cc` through `nabi`, else `nabicc`.
#[must_use]
pub fn tool() -> &'static str {
    if nabic_core::handoff_var("NABI_CLI").is_some_and(|v| v == "nabi") {
        "nabi cc"
    } else {
        "nabicc"
    }
}

/// Run the command line `args` (without the program name): the exit
/// code.
pub fn run(args: &[String]) -> i32 {
    match drive(args) {
        Ok(()) => 0,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}");
            }
            1
        }
    }
}

fn drive(args: &[String]) -> Result<(), String> {
    let tool = tool();
    let f = Flags::parse(args).map_err(|e| format!("{tool}: error: {e}"))?;
    if f.help {
        print!("{USAGE}");
        return Ok(());
    }
    if f.version {
        println!("nabicc {VERSION}");
        if f.verbose {
            println!("host: {}", env!("NABI_HOST"));
        }
        return Ok(());
    }
    let t = resolve_target(f.target.as_deref()).map_err(|e| format!("{tool}: error: {e}"))?;
    if let Some(d) = &f.dump {
        match d.as_str() {
            "-dumpmachine" => println!("{}", t.c_triple()),
            "-dumpversion" | "-dumpfullversion" => println!("{}", env!("CARGO_PKG_VERSION")),
            _ => println!(),
        }
        return Ok(());
    }
    if f.sources.is_empty() && (f.mode != Mode::Link || f.link.is_empty()) {
        return Err(format!("{tool}: error: no input files"));
    }
    let single = f.sources.len() == 1;
    if f.output.is_some() && !single && f.mode != Mode::Link {
        return Err(format!(
            "{tool}: error: `-o` with -c, -S or -E takes one source file; compile each \
             separately"
        ));
    }
    let obj_ext = if t.object_format == nabic_target::ObjectFormat::Coff {
        "obj"
    } else {
        "o"
    };
    // The sources over as many threads as the machine has cores.
    let n = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<Result<unit::Output, String>>>> = f
        .sources
        .iter()
        .map(|_| std::sync::Mutex::new(None))
        .collect();
    std::thread::scope(|s| {
        for _ in 0..n.min(f.sources.len()) {
            let (f, t, next, slots) = (&f, &t, &next, &slots);
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn_scoped(s, move || {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(p) = f.sources.get(i) else { break };
                        let o = unit::compile(p, f, t);
                        *slots[i]
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(o);
                    }
                })
                .expect("a compiler thread");
        }
    });
    let outs = slots.into_iter().map(|m| {
        m.into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unwrap_or_else(|| Err("a compiler thread stopped".to_owned()))
    });
    let mut failed = false;
    let mut objects = Vec::new();
    let tmp = std::env::temp_dir().join(format!("nabicc-{}", std::process::id()));
    for (src, o) in f.sources.iter().zip(outs) {
        let o = match o {
            Ok(o) => o,
            Err(e) => {
                eprintln!("{e}");
                failed = true;
                continue;
            }
        };
        let stem = src
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let target_name = |ext: &str| {
            f.output
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("{stem}.{ext}")))
        };
        if let Some(d) = &f.deps {
            let obj = target_name(obj_ext);
            write_deps(d, &obj, &o.files, f.output.as_deref(), src)?;
            if d.only {
                continue;
            }
        }
        match f.mode {
            Mode::Preprocess => match &f.output {
                Some(p) => {
                    std::fs::write(p, &o.bytes).map_err(|e| format!("{}: {e}", p.display()))?;
                }
                None => {
                    let _ = std::io::stdout().write_all(&o.bytes);
                }
            },
            Mode::Asm => {
                let p = target_name("s");
                std::fs::write(&p, &o.bytes).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            Mode::Compile => {
                let p = target_name(obj_ext);
                std::fs::write(&p, &o.bytes).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            Mode::Link => {
                std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
                let p = tmp.join(format!("{}-{stem}.{obj_ext}", objects.len()));
                std::fs::write(&p, &o.bytes).map_err(|e| format!("{}: {e}", p.display()))?;
                objects.push(p);
            }
        }
    }
    if failed {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(String::new());
    }
    if f.mode != Mode::Link || f.deps.as_ref().is_some_and(|d| d.only) {
        return Ok(());
    }
    if f.shared {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "{tool}: error: `-shared`: `nabi cc` links executables; build a static library with \
             `ar` (or `nabi build` a package)"
        ));
    }
    let exe = if t.object_format == nabic_target::ObjectFormat::Coff {
        "a.exe"
    } else {
        "a.out"
    };
    let out = f.output.clone().unwrap_or_else(|| PathBuf::from(exe));
    let mut inputs: Vec<OsString> = objects.iter().map(|p| p.clone().into_os_string()).collect();
    inputs.extend(f.link.iter().map(OsString::from));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let r = link::executable(&t, &inputs, &out, &tmp, f.debug);
    let _ = std::fs::remove_dir_all(&tmp);
    r.map_err(|e| format!("{tool}: error: the link failed:\n{e}"))
}

/// A make rule: `obj: src headers...`.
fn write_deps(
    d: &flags::Deps,
    obj: &Path,
    files: &[(PathBuf, bool)],
    out: Option<&Path>,
    src: &Path,
) -> Result<(), String> {
    let targets = if d.targets.is_empty() {
        obj.display().to_string()
    } else {
        d.targets.join(" ")
    };
    let mut s = format!("{targets}:");
    let mut deps = Vec::new();
    for (p, system) in files {
        if *system && !d.system {
            continue;
        }
        let p = p.display().to_string();
        if !deps.contains(&p) {
            deps.push(p);
        }
    }
    for p in &deps {
        s.push_str(" \\\n  ");
        s.push_str(&p.replace(' ', "\\ "));
    }
    s.push('\n');
    if d.phony {
        for p in deps.iter().skip(1) {
            s.push_str(&format!("\n{}:\n", p.replace(' ', "\\ ")));
        }
    }
    let path = match (&d.file, d.only) {
        (Some(p), _) => p.clone(),
        (None, true) => {
            print!("{s}");
            return Ok(());
        }
        (None, false) => {
            let base = out.map_or_else(
                || PathBuf::from(src.file_stem().unwrap_or_default()),
                Path::to_path_buf,
            );
            base.with_extension("d")
        }
    };
    std::fs::write(&path, s).map_err(|e| format!("{}: {e}", path.display()))
}
