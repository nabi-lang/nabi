//! Dynamic libraries to link against: `.tbd` stubs and Mach-O dylibs.
//!
//! Apple's text stubs are TAPI v1 to v4 (the YAML subset they use); a
//! dylib gives its export trie. Re-exported libraries fold into the
//! library that re-exports them, which is what the executable names.

use crate::bytes::u32le;
use nabic_core::HashSet;
use nabic_core::leb::read_uleb;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Dylib {
    pub install_name: String,
    pub current: u32,
    pub compat: u32,
    pub exports: HashSet<String>,
}

/// `X[.Y[.Z]]` packed as `X << 16 | Y << 8 | Z`.
#[must_use]
pub fn parse_version(s: &str) -> u32 {
    let mut it = s.trim().split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let x = it.next().unwrap_or(0);
    let y = it.next().unwrap_or(0);
    let z = it.next().unwrap_or(0);
    (x << 16) | ((y & 0xff) << 8) | (z & 0xff)
}

/// What a link wants of a dylib: an `-arch` and a `PLATFORM_*`.
#[derive(Clone, Copy, Debug)]
pub struct Want<'a> {
    pub arch: &'a str,
    pub platform: u32,
}

impl<'a> Want<'a> {
    #[must_use]
    pub const fn macos(arch: &'a str) -> Self {
        Self { arch, platform: 1 }
    }
}

/// Whether a `.tbd` target (`arm64e-macos`, `arm64-ios-simulator`, `arm64`:
/// macOS) serves `w`: arm64 clients link against arm64e stubs as ld64 does.
fn target_matches(t: &str, w: Want<'_>) -> bool {
    let (a, platform) = t.split_once('-').unwrap_or((t, "macos"));
    let arch_ok = match w.arch {
        "arm64" => a == "arm64" || a.starts_with("arm64e"),
        other => a == other,
    };
    let platform_ok = match w.platform {
        2 => platform == "ios",
        7 => platform == "ios-simulator",
        _ => platform == "macos" || platform == "macosx",
    };
    arch_ok && platform_ok
}

/// A YAML scalar or flow sequence, unquoted.
fn values(v: &str) -> Vec<String> {
    let v = v.trim();
    let inner = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(v);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => {
                if q == '\'' && chars.peek() == Some(&'\'') {
                    cur.push('\'');
                    chars.next();
                } else {
                    quote = None;
                }
            }
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == ',' => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_owned());
                }
                cur.clear();
            }
            Some(_) | None => cur.push(c),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_owned());
    }
    out
}

/// `(indent, is list item, key, value)` per logical line; flow sequences
/// spanning lines are joined.
fn entries(doc: &str) -> Vec<(usize, bool, String, String)> {
    let mut out: Vec<(usize, bool, String, String)> = Vec::new();
    let mut open = 0i32;
    for line in doc.lines() {
        if open > 0 {
            if let Some(last) = out.last_mut() {
                last.3.push(' ');
                last.3.push_str(line.trim());
            }
            open += bracket_depth(line);
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut indent = line.len() - trimmed.len();
        let (item, rest) = match trimmed.strip_prefix("- ") {
            Some(r) => {
                indent += 2;
                (true, r.trim_start())
            }
            None => (false, trimmed),
        };
        let Some((k, v)) = rest.split_once(':') else {
            continue;
        };
        open = bracket_depth(v);
        out.push((indent, item, k.trim().to_owned(), v.trim().to_owned()));
    }
    out
}

fn bracket_depth(s: &str) -> i32 {
    let mut d = 0;
    let mut quote = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '[' => d += 1,
            None if c == ']' => d -= 1,
            Some(_) | None => {}
        }
    }
    d
}

/// One `--- !tapi-tbd` document.
#[derive(Default)]
struct Doc {
    install_name: String,
    current: u32,
    compat: u32,
    exports: Vec<String>,
    reexports: Vec<String>,
}

fn parse_doc(text: &str, w: Want<'_>) -> Doc {
    // The simulator SDK's libSystem re-exports the Mac's own kernel
    // library (`/usr/lib/system/libsystem_kernel.dylib`, macOS targets
    // only): a simulator process runs on the macOS kernel. Only libSystem's
    // parts: macOS stubs of anything else do not serve a simulator link.
    let system_part = entries(text).into_iter().any(|(indent, _, k, v)| {
        indent == 0
            && k == "install-name"
            && values(&v).iter().any(|n| n.starts_with("/usr/lib/system/"))
    });
    if w.platform == 7 && system_part {
        let doc: Vec<String> = entries(text)
            .into_iter()
            .filter(|(indent, _, k, _)| *indent == 0 && k == "targets")
            .flat_map(|(_, _, _, v)| values(&v))
            .collect();
        let mac = Want::macos(w.arch);
        if !doc.iter().any(|t| target_matches(t, w)) && doc.iter().any(|t| target_matches(t, mac)) {
            return parse_doc(text, mac);
        }
    }
    let mut d = Doc {
        current: 0x10000,
        compat: 0x10000,
        ..Doc::default()
    };
    // The section a list item belongs to, and the current item's state.
    let mut section = String::new();
    let mut item_ok = true;
    let mut doc_archs: Option<Vec<String>> = None;
    for (indent, item, k, v) in entries(text) {
        if indent == 0 {
            section.clone_from(&k);
            match k.as_str() {
                "install-name" => d.install_name = values(&v).pop().unwrap_or_default(),
                "current-version" => {
                    d.current = parse_version(&values(&v).pop().unwrap_or_default());
                }
                "compatibility-version" => {
                    d.compat = parse_version(&values(&v).pop().unwrap_or_default());
                }
                "archs" => doc_archs = Some(values(&v)),
                _ => {}
            }
            continue;
        }
        if item {
            item_ok = true;
        }
        match k.as_str() {
            "targets" | "archs" => {
                item_ok = values(&v).iter().any(|t| target_matches(t, w));
            }
            _ if !item_ok => {}
            "symbols" | "weak-symbols" | "weak-def-symbols" | "thread-local-symbols" => {
                if section == "exports" || section == "reexports" {
                    d.exports.extend(values(&v));
                }
            }
            "objc-classes" => {
                for c in values(&v) {
                    let c = c.trim_start_matches('_');
                    d.exports.push(format!("_OBJC_CLASS_$_{c}"));
                    d.exports.push(format!("_OBJC_METACLASS_$_{c}"));
                }
            }
            "objc-eh-types" => {
                d.exports
                    .extend(values(&v).iter().map(|c| format!("_OBJC_EHTYPE_$_{c}")));
            }
            "objc-ivars" => {
                d.exports
                    .extend(values(&v).iter().map(|c| format!("_OBJC_IVAR_$_{c}")));
            }
            "libraries" | "re-exports" => d.reexports.extend(values(&v)),
            _ => {}
        }
    }
    if doc_archs.is_some_and(|a| !a.iter().any(|t| target_matches(t, w))) {
        d.exports.clear();
        d.reexports.clear();
    }
    // ld64's per-OS-version directives are not symbols.
    d.exports.retain(|s| !s.starts_with("$ld$"));
    d
}

/// Read the `.tbd` at `path`; re-exported libraries come from its other
/// documents or, failing that, `.tbd` files under `sysroot`.
pub fn read_tbd(path: &Path, w: Want<'_>, sysroot: Option<&Path>) -> Result<Dylib, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if text.trim_start().starts_with('{') {
        return Err(format!(
            "{}: JSON .tbd (TAPI v5) is not supported",
            path.display()
        ));
    }
    let docs: Vec<Doc> = text
        .split("\n---")
        .map(|d| d.trim_end_matches("...").trim_end_matches("\n..."))
        .map(|d| {
            // Drop the `--- !tapi-tbd` tag line.
            d.split_once('\n').map_or("", |(_, rest)| rest)
        })
        .map(|d| parse_doc(d, w))
        .collect();
    let Some(first) = docs.first() else {
        return Err(format!("{}: empty .tbd", path.display()));
    };
    let mut lib = Dylib {
        install_name: first.install_name.clone(),
        current: first.current,
        compat: first.compat,
        exports: HashSet::default(),
    };
    let mut seen = HashSet::default();
    let mut work = vec![first.install_name.clone()];
    while let Some(name) = work.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(d) = docs.iter().find(|d| d.install_name == name) {
            lib.exports.extend(d.exports.iter().cloned());
            work.extend(d.reexports.iter().cloned());
        } else if let Some(sub) = find_installed(&name, sysroot) {
            let sub = read_dylib(&sub, w, sysroot)?;
            lib.exports.extend(sub.exports);
        }
    }
    Ok(lib)
}

/// The stub or library file of install name `name` under `sysroot`.
fn find_installed(name: &str, sysroot: Option<&Path>) -> Option<PathBuf> {
    let rel = name.trim_start_matches('/');
    let root = sysroot.map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
    let p = root.join(rel);
    let tbd = p.with_extension("tbd");
    if tbd.is_file() {
        return Some(tbd);
    }
    p.is_file().then_some(p)
}

/// A `.tbd` or a Mach-O dylib (thin or universal).
pub fn read_dylib(path: &Path, w: Want<'_>, sysroot: Option<&Path>) -> Result<Dylib, String> {
    if path.extension().is_some_and(|e| e == "tbd") {
        return read_tbd(path, w, sysroot);
    }
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let d = crate::macho::object::thin_slice(&data, w.arch)
        .ok_or_else(|| format!("{}: no {} slice", path.display(), w.arch))?;
    parse_macho_dylib(d, w, sysroot).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_macho_dylib(d: &[u8], w: Want<'_>, sysroot: Option<&Path>) -> Result<Dylib, String> {
    if u32le(d, 0) != 0xfeed_facf || u32le(d, 12) != 6 {
        return Err("not a 64-bit Mach-O dylib".to_owned());
    }
    let ncmds = u32le(d, 16);
    let mut lib = Dylib::default();
    let mut trie: Option<(usize, usize)> = None;
    let mut reexports = Vec::new();
    let mut at = 32;
    for _ in 0..ncmds {
        let (cmd, size) = (u32le(d, at), u32le(d, at + 4) as usize);
        match cmd {
            // LC_ID_DYLIB, LC_REEXPORT_DYLIB
            0xd | 0x8000_001f => {
                let name = crate::bytes::cstr(d, at + u32le(d, at + 8) as usize).to_owned();
                if cmd == 0xd {
                    lib.install_name = name;
                    lib.current = u32le(d, at + 16);
                    lib.compat = u32le(d, at + 20);
                } else {
                    reexports.push(name);
                }
            }
            // LC_DYLD_INFO(_ONLY): the export trie's offset and size.
            0x22 | 0x8000_0022 => {
                let (off, n) = (u32le(d, at + 40) as usize, u32le(d, at + 44) as usize);
                if n > 0 {
                    trie = Some((off, n));
                }
            }
            // LC_DYLD_EXPORTS_TRIE
            0x8000_0033 => trie = Some((u32le(d, at + 8) as usize, u32le(d, at + 12) as usize)),
            _ => {}
        }
        if size == 0 {
            break;
        }
        at += size;
    }
    if let Some((off, n)) = trie {
        let t = d.get(off..off + n).ok_or("export trie out of bounds")?;
        walk_trie(t, 0, &mut Vec::new(), &mut lib.exports, 0);
    }
    for r in reexports {
        if let Some(p) = find_installed(&r, sysroot).or_else(|| {
            let p = PathBuf::from(&r);
            p.is_file().then_some(p)
        }) {
            lib.exports.extend(read_dylib(&p, w, sysroot)?.exports);
        }
    }
    Ok(lib)
}

fn walk_trie(t: &[u8], node: usize, prefix: &mut Vec<u8>, out: &mut HashSet<String>, depth: u32) {
    if depth > 4096 {
        return;
    }
    let mut at = node;
    let Some(term) = read_uleb(t, &mut at) else {
        return;
    };
    if term > 0 {
        out.insert(String::from_utf8_lossy(prefix).into_owned());
    }
    at += term as usize;
    let Some(&n) = t.get(at) else {
        return;
    };
    at += 1;
    for _ in 0..n {
        let start = at;
        while t.get(at).is_some_and(|&c| c != 0) {
            at += 1;
        }
        let edge = t[start..at.min(t.len())].to_vec();
        at += 1;
        let Some(child) = read_uleb(t, &mut at) else {
            return;
        };
        let len = prefix.len();
        prefix.extend_from_slice(&edge);
        walk_trie(t, child as usize, prefix, out, depth + 1);
        prefix.truncate(len);
    }
}

#[cfg(test)]
pub fn walk_trie_for_test(t: &[u8], d: &mut Dylib) {
    walk_trie(t, 0, &mut Vec::new(), &mut d.exports, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    const TBD: &str = "--- !tapi-tbd
tbd-version:     4
targets:         [ x86_64-macos, arm64e-macos ]
install-name:    '/usr/lib/libSystem.B.dylib'
current-version: 1351.2
reexported-libraries:
  - targets:         [ x86_64-macos, arm64e-macos ]
    libraries:       [ '/usr/lib/system/libsystem_c.dylib' ]
exports:
  - targets:         [ x86_64-macos ]
    symbols:         [ _only_intel ]
  - targets:         [ x86_64-macos, arm64e-macos,
                       arm64e.x1-macos ]
    symbols:         [ ___crashreporter_info__, '$ld$hide$os10.4$_x',
                       _mach_init_routine ]
--- !tapi-tbd
tbd-version:     4
targets:         [ arm64-macos, arm64e-macos ]
install-name:    '/usr/lib/system/libsystem_c.dylib'
exports:
  - targets:         [ arm64-macos, arm64e-macos ]
    symbols:         [ _printf, _puts ]
    weak-symbols:    [ _weakling ]
    objc-classes:    [ Thing ]
...
";

    #[test]
    fn tbd_folds_reexports_for_the_arch() {
        let dir = nabic_core::tmp::test_dir("link-tbd");
        let p = dir.join("libSystem.tbd");
        std::fs::write(&p, TBD).unwrap();
        let lib = read_tbd(&p, Want::macos("arm64"), None).unwrap();
        assert_eq!(lib.install_name, "/usr/lib/libSystem.B.dylib");
        assert_eq!(lib.current, parse_version("1351.2"));
        let mut e: Vec<&str> = lib.exports.iter().map(String::as_str).collect();
        e.sort_unstable();
        assert_eq!(
            e,
            [
                "_OBJC_CLASS_$_Thing",
                "_OBJC_METACLASS_$_Thing",
                "___crashreporter_info__",
                "_mach_init_routine",
                "_printf",
                "_puts",
                "_weakling"
            ]
        );
        let intel = read_tbd(&p, Want::macos("x86_64"), None).unwrap();
        assert!(intel.exports.contains("_only_intel"));
        assert!(!intel.exports.contains("_printf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    const SIM_TBD: &str = "--- !tapi-tbd
tbd-version:     4
targets:         [ arm64-ios-simulator ]
install-name:    '/usr/lib/libSystem.B.dylib'
reexported-libraries:
  - targets:         [ arm64-ios-simulator ]
    libraries:       [ '/usr/lib/system/libsystem_kernel.dylib' ]
exports:
  - targets:         [ arm64-ios-simulator ]
    symbols:         [ _puts ]
--- !tapi-tbd
tbd-version:     4
targets:         [ arm64-macos, arm64e-macos ]
install-name:    '/usr/lib/system/libsystem_kernel.dylib'
exports:
  - targets:         [ arm64-macos, arm64e-macos ]
    symbols:         [ _kevent ]
...
";

    /// The simulator's libSystem re-exports the Mac's kernel library.
    #[test]
    fn simulator_takes_the_macos_kernel() {
        let dir = nabic_core::tmp::test_dir("link-tbd-sim");
        let p = dir.join("libSystem.tbd");
        std::fs::write(&p, SIM_TBD).unwrap();
        let sim = read_tbd(
            &p,
            Want {
                arch: "arm64",
                platform: 7,
            },
            None,
        )
        .unwrap();
        assert!(sim.exports.contains("_puts") && sim.exports.contains("_kevent"));
        let dev = read_tbd(
            &p,
            Want {
                arch: "arm64",
                platform: 2,
            },
            None,
        )
        .unwrap();
        assert!(dev.exports.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_pack() {
        assert_eq!(parse_version("1.2.12"), 0x0001_020c);
        assert_eq!(parse_version("1359"), 1359 << 16);
    }
}
