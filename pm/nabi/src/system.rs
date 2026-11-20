//! `Dep::system(..)` and `Dep::framework()`: C libraries installed on the
//! machine, found at build time without running any program.
//!
//! Where, in order: pkg-config `.pc` files (read here, no `pkg-config`
//! binary), Homebrew's `opt/<formula>`, vcpkg's `installed/<triplet>`,
//! the macOS SDK's `usr/lib/*.tbd`. A cross build looks only where the
//! target's files live (`PKG_CONFIG_SYSROOT_DIR`, the target's multiarch
//! directory, `PKG_CONFIG_PATH` / `PKG_CONFIG_LIBDIR`), never at the
//! host's Homebrew.

use crate::manifest::{NativeDep, NativeSource, SystemLib};
use nabic_target::{Libc, ObjectFormat, TargetSpec};
use std::path::{Path, PathBuf};

/// What a found library adds to a build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub lib_dirs: Vec<PathBuf>,
    /// `(name, static)`.
    pub libs: Vec<(String, bool)>,
    /// Header directories and `-D` macros, for C sources and imports.
    pub include: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    /// Shared libraries copied beside the executable (vcpkg's DLLs).
    pub runtime: Vec<PathBuf>,
    /// The `.pc` file's `Version`.
    pub version: Option<String>,
    /// Where it was found, for messages.
    pub from: String,
}

/// nabic arguments linking `deps` for `t`.
pub fn nabic_args(deps: &[NativeDep], t: &TargetSpec) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for d in deps {
        match &d.source {
            NativeSource::Framework if t.object_format == ObjectFormat::MachO => {
                out.push(format!("-lframework={}", d.name));
            }
            NativeSource::System(s) => {
                let f = find(&d.name, s, t, &Env::real())?;
                for dir in &f.lib_dirs {
                    out.push(format!("-L{}", dir.display()));
                }
                for (n, stat) in &f.libs {
                    out.push(format!("-l{}={n}", if *stat { "static" } else { "dylib" }));
                }
                for r in &f.runtime {
                    out.push(format!("--native-runtime={}", r.display()));
                }
            }
            // A framework is Apple's: other targets build without it.
            _ => {}
        }
    }
    Ok(out)
}

/// An environment variable's value, if set and not empty.
pub type Var = Box<dyn Fn(&str) -> Option<String>>;

/// The environment discovery reads, so tests can give their own.
pub struct Env {
    pub var: Var,
    /// The host is macOS / Windows (Homebrew, vcpkg defaults).
    pub mac: bool,
    pub windows: bool,
    /// The target is the host's (`TargetSpec::is_host`).
    pub native: Option<bool>,
}

impl Env {
    #[must_use]
    pub fn real() -> Self {
        Self {
            var: Box::new(|k| std::env::var(k).ok().filter(|v| !v.is_empty())),
            mac: cfg!(target_os = "macos"),
            windows: cfg!(windows),
            native: None,
        }
    }
}

/// Library `name` for `t`, or an error saying how to install it.
pub fn find(name: &str, s: &SystemLib, t: &TargetSpec, env: &Env) -> Result<Found, String> {
    let module = s.pkg_config.as_deref().unwrap_or(name);
    let native = env.native.unwrap_or_else(|| t.is_host());
    let names: Vec<String> = if s.libs.is_empty() {
        vec![name.to_owned()]
    } else {
        s.libs.clone()
    };
    let mut looked = Vec::new();
    // 1. pkg-config.
    let dirs = pc_dirs(t, native, env);
    if let Some(pc) = find_pc(&dirs, module) {
        let mut f = from_pc(&pc, &dirs, s.statik, env)?;
        f.from = pc.display().to_string();
        return Ok(f);
    }
    looked.push(format!("pkg-config ({module}.pc)"));
    // 2. Homebrew, for the machine it runs on.
    if native && (env.mac || (env.var)("HOMEBREW_PREFIX").is_some()) {
        let prefix = (env.var)("HOMEBREW_PREFIX").unwrap_or_else(|| {
            if t.arch == "aarch64" {
                "/opt/homebrew".to_owned()
            } else {
                "/usr/local".to_owned()
            }
        });
        let opt = Path::new(&prefix)
            .join("opt")
            .join(s.brew.as_deref().unwrap_or(name));
        if let Some(f) = from_prefix(&opt, module, &names, s.statik, &dirs, env)? {
            return Ok(f);
        }
        looked.push("Homebrew".to_owned());
    }
    // 3. vcpkg.
    let root = (env.var)("VCPKG_ROOT").or_else(|| (env.var)("VCPKG_INSTALLATION_ROOT"));
    if let (Some(root), Some(triplet)) = (root, triplet(t, s.statik, env)) {
        let dir = Path::new(&root).join("installed").join(&triplet);
        if let Some(mut f) = from_prefix(&dir, module, &names, s.statik, &dirs, env)? {
            if t.object_format == ObjectFormat::Coff && !s.statik {
                f.runtime = dlls(&dir.join("bin"), &f.libs);
            }
            return Ok(f);
        }
        looked.push(format!("vcpkg ({})", dir.display()));
    }
    // 4. The macOS SDK's own libraries (libz, libsqlite3, ...).
    if t.object_format == ObjectFormat::MachO {
        if let Ok(sdk) = nabic_target::apple::sdk_for(t) {
            let lib = sdk.join("usr/lib");
            if names
                .iter()
                .all(|n| lib.join(format!("lib{n}.tbd")).is_file())
            {
                return Ok(Found {
                    libs: names.iter().map(|n| (n.clone(), false)).collect(),
                    include: vec![sdk.join("usr/include")],
                    from: sdk.display().to_string(),
                    ..Found::default()
                });
            }
            looked.push("the macOS SDK".to_owned());
        }
    }
    Err(not_found(name, module, s, t, native, &looked, env))
}

fn not_found(
    name: &str,
    module: &str,
    s: &SystemLib,
    t: &TargetSpec,
    native: bool,
    looked: &[String],
    env: &Env,
) -> String {
    let what = if module == name {
        format!("system library `{name}`")
    } else {
        format!("system library `{name}` (pkg-config `{module}`)")
    };
    let install = if !native {
        format!(
            "install it for {} and set PKG_CONFIG_SYSROOT_DIR to that sysroot, or \
             PKG_CONFIG_PATH to the directory holding {module}.pc",
            t.name
        )
    } else if env.mac {
        format!(
            "install it with `brew install {}`, or set PKG_CONFIG_PATH to the directory holding \
             {module}.pc",
            s.brew.as_deref().unwrap_or(name)
        )
    } else if env.windows {
        format!(
            "install it with `vcpkg install {}:{}` and set VCPKG_ROOT, or set PKG_CONFIG_PATH to \
             the directory holding {module}.pc",
            s.vcpkg.as_deref().unwrap_or(name),
            triplet(t, s.statik, env).unwrap_or_else(|| "x64-windows".to_owned())
        )
    } else if let Some(apt) = &s.apt {
        format!(
            "install it with `sudo apt install {apt}` (or your distribution's package of it), or \
             set PKG_CONFIG_PATH to the directory holding {module}.pc"
        )
    } else {
        format!(
            "install its development package (the one with {module}.pc), or set PKG_CONFIG_PATH \
             to the directory holding {module}.pc"
        )
    };
    format!(
        "{what} not found for {} (looked in {}): {install}",
        t.name,
        looked.join(", ")
    )
}

/// The vcpkg triplet of `t`; `VCPKG_DEFAULT_TRIPLET` wins.
fn triplet(t: &TargetSpec, statik: bool, env: &Env) -> Option<String> {
    if let Some(t) = (env.var)("VCPKG_DEFAULT_TRIPLET") {
        return Some(t);
    }
    let arch = match t.arch.as_str() {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        "arm" => "arm",
        _ => return None,
    };
    let os = match t.object_format {
        ObjectFormat::Coff => {
            return Some(format!(
                "{arch}-windows{}",
                if statik { "-static" } else { "" }
            ));
        }
        ObjectFormat::MachO => "osx",
        _ if t.os == "linux" => "linux",
        _ => return None,
    };
    Some(format!("{arch}-{os}"))
}

/// A prefix (`opt/<formula>`, `installed/<triplet>`): its own `.pc`
/// file, else its `lib/` and `include/` with `names` linked.
fn from_prefix(
    dir: &Path,
    module: &str,
    names: &[String],
    statik: bool,
    dirs: &[PathBuf],
    env: &Env,
) -> Result<Option<Found>, String> {
    if !dir.is_dir() {
        return Ok(None);
    }
    for sub in ["lib/pkgconfig", "share/pkgconfig"] {
        let pc = dir.join(sub).join(format!("{module}.pc"));
        if pc.is_file() {
            let mut search = vec![dir.join(sub)];
            search.extend(dirs.iter().cloned());
            let mut f = from_pc(&pc, &search, statik, env)?;
            f.from = pc.display().to_string();
            return Ok(Some(f));
        }
    }
    let lib = dir.join("lib");
    if !lib.is_dir() {
        return Ok(None);
    }
    Ok(Some(Found {
        lib_dirs: vec![lib],
        libs: names.iter().map(|n| (n.clone(), statik)).collect(),
        include: vec![dir.join("include")],
        from: dir.display().to_string(),
        ..Found::default()
    }))
}

/// The DLLs in `bin` the import libraries `libs` stand for: `zlib1.dll`
/// for `zlib`.
fn dlls(bin: &Path, libs: &[(String, bool)]) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(bin) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            Path::new(&n).extension().is_some_and(|x| x == "dll")
                && libs
                    .iter()
                    .any(|(l, _)| n.starts_with(&l.to_ascii_lowercase()))
        })
        .collect();
    out.sort();
    out
}

/// The `.pc` search path for `t`: `PKG_CONFIG_PATH`, then
/// `PKG_CONFIG_LIBDIR` or the default directories (the target's
/// multiarch one only, for a cross build).
pub fn pc_dirs(t: &TargetSpec, native: bool, env: &Env) -> Vec<PathBuf> {
    let split = |v: String| -> Vec<PathBuf> {
        std::env::split_paths(&v)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    };
    let mut out = (env.var)("PKG_CONFIG_PATH").map(split).unwrap_or_default();
    if let Some(l) = (env.var)("PKG_CONFIG_LIBDIR") {
        out.extend(split(l));
        return out;
    }
    let sysroot = (env.var)("PKG_CONFIG_SYSROOT_DIR").unwrap_or_default();
    let at = |p: &str| PathBuf::from(format!("{sysroot}{p}"));
    if let Some(m) = multiarch(t) {
        out.push(at(&format!("/usr/lib/{m}/pkgconfig")));
    }
    if native || !sysroot.is_empty() {
        for d in [
            "/usr/local/lib/pkgconfig",
            "/usr/local/share/pkgconfig",
            "/usr/lib64/pkgconfig",
            "/usr/lib/pkgconfig",
            "/usr/share/pkgconfig",
        ] {
            out.push(at(d));
        }
    }
    if native && env.mac {
        let brew = (env.var)("HOMEBREW_PREFIX").unwrap_or_else(|| "/opt/homebrew".to_owned());
        out.push(Path::new(&brew).join("lib/pkgconfig"));
        out.push(Path::new(&brew).join("share/pkgconfig"));
    }
    out
}

/// Debian's multiarch directory name of `t`.
fn multiarch(t: &TargetSpec) -> Option<&'static str> {
    if t.os != "linux" || t.libc != Libc::Glibc {
        return None;
    }
    Some(match t.arch.as_str() {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        "x86" => "i386-linux-gnu",
        "arm" => "arm-linux-gnueabihf",
        "riscv64" => "riscv64-linux-gnu",
        "powerpc64" => "powerpc64le-linux-gnu",
        "s390x" => "s390x-linux-gnu",
        _ => return None,
    })
}

fn find_pc(dirs: &[PathBuf], module: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(format!("{module}.pc")))
        .find(|p| p.is_file())
}

/// A parsed `.pc` file.
#[derive(Debug, Default)]
pub struct Pc {
    pub version: Option<String>,
    pub libs: Vec<String>,
    pub libs_private: Vec<String>,
    pub cflags: Vec<String>,
    pub requires: Vec<String>,
    pub requires_private: Vec<String>,
}

/// `.pc` text; `dir` is its directory (`${pcfiledir}`).
pub fn parse_pc(text: &str, dir: &Path) -> Pc {
    let mut vars: Vec<(String, String)> = vec![("pcfiledir".to_owned(), dir.display().to_string())];
    let mut pc = Pc::default();
    // Lines may continue with a trailing `\`.
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    for line in joined.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let colon = line.find(':');
        let eq = line.find('=');
        match (colon, eq) {
            (Some(c), e) if e.is_none_or(|e| c < e) => {
                let key = line[..c].trim();
                let v = expand(line[c + 1..].trim(), &vars);
                let words = split_words(&v);
                match key {
                    "Version" => pc.version = Some(v),
                    "Libs" => pc.libs = words,
                    "Libs.private" => pc.libs_private = words,
                    "Cflags" | "CFlags" => pc.cflags = words,
                    "Requires" => pc.requires = modules(&v),
                    "Requires.private" => pc.requires_private = modules(&v),
                    _ => {}
                }
            }
            (_, Some(e)) => {
                let key = line[..e].trim().to_owned();
                let v = expand(line[e + 1..].trim(), &vars);
                vars.retain(|(k, _)| *k != key);
                vars.push((key, v));
            }
            _ => {}
        }
    }
    pc
}

fn expand(s: &str, vars: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let name = &rest[i + 2..i + j];
        if let Some((_, v)) = vars.iter().find(|(k, _)| k == name) {
            out.push_str(v);
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

/// Shell-like words: spaces split, quotes and `\` group.
fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut any = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (_, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (_, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Module names of a `Requires` value: `glib-2.0 >= 2.50, zlib`.
fn modules(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for w in v.split(|c: char| c == ',' || c.is_whitespace()) {
        if w.is_empty() {
            continue;
        }
        if skip {
            skip = false;
        } else if matches!(w, "=" | "<" | ">" | "<=" | ">=" | "!=") {
            skip = true;
        } else {
            out.push(w.to_owned());
        }
    }
    out
}

/// What `.pc` file `pc` and the files it requires add; `statik` takes
/// the private parts too.
fn from_pc(pc: &Path, dirs: &[PathBuf], statik: bool, env: &Env) -> Result<Found, String> {
    let sysroot = (env.var)("PKG_CONFIG_SYSROOT_DIR").unwrap_or_default();
    let mut f = Found::default();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut todo = vec![pc.to_path_buf()];
    let mut first = true;
    while let Some(p) = todo.pop() {
        if seen.contains(&p) {
            continue;
        }
        seen.push(p.clone());
        let text =
            std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        let dir = p.parent().unwrap_or_else(|| Path::new("."));
        let parsed = parse_pc(&text, dir);
        if first {
            f.version.clone_from(&parsed.version);
            first = false;
        }
        let mut req = parsed.requires.clone();
        if statik {
            req.extend(parsed.requires_private.iter().cloned());
        }
        for m in req {
            let next = find_pc(dirs, &m).ok_or_else(|| {
                format!(
                    "{} requires pkg-config module `{m}`, which is not installed (no {m}.pc in \
                     the pkg-config path)",
                    p.display()
                )
            })?;
            todo.push(next);
        }
        let mut libs = parsed.libs.clone();
        if statik {
            libs.extend(parsed.libs_private.iter().cloned());
        }
        let rooted = |d: &str| {
            if sysroot.is_empty() || !d.starts_with('/') || d.starts_with(&sysroot) {
                PathBuf::from(d)
            } else {
                PathBuf::from(format!("{sysroot}{d}"))
            }
        };
        let mut it = libs.iter();
        while let Some(w) = it.next() {
            if let Some(d) = w.strip_prefix("-L") {
                let d = if d.is_empty() {
                    it.next().map_or("", |s| s.as_str())
                } else {
                    d
                };
                let d = rooted(d);
                if !d.as_os_str().is_empty() && !f.lib_dirs.contains(&d) {
                    f.lib_dirs.push(d);
                }
            } else if let Some(n) = w.strip_prefix("-l") {
                let n = if n.is_empty() {
                    it.next().map_or("", |s| s.as_str())
                } else {
                    n
                };
                if !n.is_empty() && !f.libs.iter().any(|(m, _)| m == n) {
                    f.libs.push((n.to_owned(), statik));
                }
            }
        }
        let mut it = parsed.cflags.iter();
        while let Some(w) = it.next() {
            if let Some(d) = w.strip_prefix("-I") {
                let d = if d.is_empty() {
                    it.next().map_or("", |s| s.as_str())
                } else {
                    d
                };
                let d = rooted(d);
                if !d.as_os_str().is_empty() && !f.include.contains(&d) {
                    f.include.push(d);
                }
            } else if let Some(m) = w.strip_prefix("-D") {
                let (k, v) = match m.split_once('=') {
                    Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
                    None => (m.to_owned(), None),
                };
                if !f.defines.iter().any(|(x, _)| *x == k) {
                    f.defines.push((k, v));
                }
            }
        }
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)], mac: bool) -> Env {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Env {
            var: Box::new(move |k| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())),
            mac,
            windows: false,
            native: Some(true),
        }
    }

    fn linux() -> TargetSpec {
        TargetSpec::builtin("x86_64-unknown-linux-gnu").unwrap()
    }

    #[test]
    fn parses_pc_files() {
        let pc = parse_pc(
            "prefix=/opt/z\nexec_prefix=${prefix}\nlibdir=${exec_prefix}/lib # here\n\
             includedir=${prefix}/include\n\nName: zlib\nVersion: 1.3.1\n\
             Requires: a >= 1.0, b\nRequires.private: c\nLibs: -L${libdir} -lz \\\n -pthread\n\
             Libs.private: -lm\nCflags: -I${includedir} -DZ_X=1 \"-I/with space\"\n",
            Path::new("/pc"),
        );
        assert_eq!(pc.version.as_deref(), Some("1.3.1"));
        assert_eq!(pc.libs, ["-L/opt/z/lib", "-lz", "-pthread"]);
        assert_eq!(pc.libs_private, ["-lm"]);
        assert_eq!(pc.cflags, ["-I/opt/z/include", "-DZ_X=1", "-I/with space"]);
        assert_eq!(pc.requires, ["a", "b"]);
        assert_eq!(pc.requires_private, ["c"]);
        let pc = parse_pc(
            "prefix=${pcfiledir}/../..\nLibs: -L${prefix}/lib -lq\n",
            Path::new("/v/lib/pkgconfig"),
        );
        assert_eq!(pc.libs, ["-L/v/lib/pkgconfig/../../lib", "-lq"]);
    }

    #[test]
    fn finds_through_pkg_config_and_requires() {
        let dir = nabic_core::tmp::test_dir("system-pc");
        std::fs::write(
            dir.join("top.pc"),
            "prefix=/p\nVersion: 2.0\nRequires: dep\nRequires.private: priv\nLibs: -L/p/lib -ltop\n\
             Libs.private: -lextra\nCflags: -I/p/include -DTOP\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("dep.pc"),
            "Libs: -L/d/lib -ldep\nCflags: -I/d/include\n",
        )
        .unwrap();
        std::fs::write(dir.join("priv.pc"), "Libs: -lpriv\n").unwrap();
        let e = env(&[("PKG_CONFIG_LIBDIR", &dir.display().to_string())], false);
        let s = SystemLib {
            pkg_config: Some("top".into()),
            ..SystemLib::default()
        };
        let f = find("t", &s, &linux(), &e).unwrap();
        assert_eq!(f.version.as_deref(), Some("2.0"));
        assert_eq!(
            f.lib_dirs,
            [PathBuf::from("/p/lib"), PathBuf::from("/d/lib")]
        );
        assert_eq!(f.libs, [("top".into(), false), ("dep".into(), false)]);
        assert_eq!(f.defines, [("TOP".to_owned(), None)]);
        let f = find(
            "t",
            &SystemLib {
                statik: true,
                ..s.clone()
            },
            &linux(),
            &e,
        )
        .unwrap();
        let names: Vec<&str> = f.libs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["top", "extra", "priv", "dep"]);
        assert!(f.libs.iter().all(|(_, s)| *s));
        // A sysroot prefixes the paths .pc files give.
        let e = env(
            &[
                ("PKG_CONFIG_LIBDIR", &dir.display().to_string()),
                ("PKG_CONFIG_SYSROOT_DIR", "/sys"),
            ],
            false,
        );
        let f = find("t", &s, &linux(), &e).unwrap();
        assert_eq!(f.lib_dirs[0], PathBuf::from("/sys/p/lib"));
        std::fs::remove_file(dir.join("dep.pc")).unwrap();
        let err = find("t", &s, &linux(), &e).unwrap_err();
        assert!(err.contains("requires pkg-config module `dep`"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_in_a_homebrew_keg_and_vcpkg() {
        let dir = nabic_core::tmp::test_dir("system-brew");
        let keg = dir.join("opt/zlib");
        std::fs::create_dir_all(keg.join("lib/pkgconfig")).unwrap();
        std::fs::write(
            keg.join("lib/pkgconfig/zlib.pc"),
            "prefix=/k\nVersion: 1.3\nLibs: -L${prefix}/lib -lz\n",
        )
        .unwrap();
        let e = env(
            &[
                ("HOMEBREW_PREFIX", &dir.display().to_string()),
                ("PKG_CONFIG_LIBDIR", "/nonexistent"),
            ],
            true,
        );
        let s = SystemLib {
            pkg_config: Some("zlib".into()),
            brew: Some("zlib".into()),
            ..SystemLib::default()
        };
        let mac = TargetSpec::builtin("aarch64-apple-darwin").unwrap();
        let f = find("z", &s, &mac, &e).unwrap();
        assert_eq!(f.libs, [("z".into(), false)]);
        assert!(f.from.ends_with("zlib.pc"), "{}", f.from);
        // vcpkg without a .pc: its lib/ and the names.
        let v = dir.join("vcpkg/installed/x64-linux");
        std::fs::create_dir_all(v.join("lib")).unwrap();
        let e = env(
            &[
                ("VCPKG_ROOT", &dir.join("vcpkg").display().to_string()),
                ("PKG_CONFIG_LIBDIR", "/nonexistent"),
            ],
            false,
        );
        let s = SystemLib {
            libs: vec!["png16".into()],
            ..SystemLib::default()
        };
        let f = find("png", &s, &linux(), &e).unwrap();
        assert_eq!(f.lib_dirs, [v.join("lib")]);
        assert_eq!(f.libs, [("png16".into(), false)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_found_says_how_to_install() {
        let e = env(&[("PKG_CONFIG_LIBDIR", "/nonexistent")], false);
        let s = SystemLib {
            pkg_config: Some("libpng16".into()),
            apt: Some("libpng-dev".into()),
            ..SystemLib::default()
        };
        let err = find("png", &s, &linux(), &e).unwrap_err();
        assert!(
            err.contains("system library `png` (pkg-config `libpng16`) not found"),
            "{err}"
        );
        assert!(err.contains("`sudo apt install libpng-dev`"), "{err}");
        let e = env(
            &[
                ("PKG_CONFIG_LIBDIR", "/nonexistent"),
                ("HOMEBREW_PREFIX", "/nonexistent"),
            ],
            true,
        );
        let mac = TargetSpec::builtin("aarch64-apple-darwin").unwrap();
        let s = SystemLib {
            brew: Some("libpng".into()),
            libs: vec!["nonexistent_lib_x".into()],
            ..s
        };
        let err = find("png", &s, &mac, &e).unwrap_err();
        assert!(err.contains("`brew install libpng`"), "{err}");
        // A cross build never looks at the host's directories.
        let e = Env {
            native: Some(false),
            ..env(&[], false)
        };
        let arm = TargetSpec::builtin("aarch64-unknown-linux-gnu").unwrap();
        assert_eq!(
            pc_dirs(&arm, false, &e),
            [PathBuf::from("/usr/lib/aarch64-linux-gnu/pkgconfig")]
        );
    }
}
