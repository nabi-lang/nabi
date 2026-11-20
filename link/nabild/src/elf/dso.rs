//! Shared objects (their dynamic symbols and versions) and the linker
//! scripts that stand for them (`libc.so`'s `GROUP ( ... )`).

use crate::bytes::cstr;
use crate::elf::object::Class;
use nabic_core::HashMap;
use std::path::{Path, PathBuf};

const SHT_DYNAMIC: u32 = 6;
const SHT_DYNSYM: u32 = 11;
const SHT_GNU_VERDEF: u32 = 0x6fff_fffd;
const SHT_GNU_VERSYM: u32 = 0x6fff_ffff;
const DT_SONAME: u64 = 14;
const VERSYM_HIDDEN: u16 = 0x8000;

#[derive(Clone, Copy, Debug)]
pub struct Export {
    /// Version index (0 local, 1 global: unversioned).
    pub ver: u16,
    pub ty: u8,
    pub size: u64,
}

#[derive(Debug)]
pub struct Dso {
    pub path: PathBuf,
    pub soname: String,
    pub as_needed: bool,
    /// Version names by index.
    pub verdefs: HashMap<u16, String>,
    /// Definitions an unversioned reference binds: the default versions.
    pub exports: HashMap<String, Export>,
    /// Every versioned definition, by `(name, version)`.
    pub versioned: HashMap<(String, String), Export>,
    pub undefined: Vec<String>,
}

/// Read shared object `raw` (`path`) for `machine`.
pub fn read(path: &Path, raw: &[u8], arch: crate::elf::Arch) -> Result<Dso, String> {
    let bad = |e: &str| format!("{}: {e}", path.display());
    crate::elf::object::check_header(raw, arch).map_err(|e| bad(&e))?;
    let c = Class::of(raw);
    let (hs, _) = crate::elf::object::shdrs(raw).map_err(|e| bad(&e))?;
    if hs.is_empty() {
        return Err(bad("no section headers"));
    }
    let find = |ty: u32| {
        hs.iter()
            .find(|h| h.ty() == ty)
            .map(crate::elf::object::Shdr::place)
    };
    let (sym_off, sym_size, link, _) =
        find(SHT_DYNSYM).ok_or_else(|| bad("no dynamic symbol table"))?;
    let strtab = hs.get(link as usize).map_or(0, |h| h.place().0);
    let nsyms = sym_size / c.sym_size();
    let versym = find(SHT_GNU_VERSYM).map(|h| h.0);

    let mut soname = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    if let Some((off, size, _, _)) = find(SHT_DYNAMIC) {
        let (ent, w) = (c.dyn_size(), c.word_size());
        for k in 0..size / ent {
            let tag = c.word(raw, off + k * ent);
            if tag == 0 {
                break;
            }
            if tag == DT_SONAME {
                cstr(raw, strtab + c.word(raw, off + k * ent + w) as usize).clone_into(&mut soname);
            }
        }
    }

    let mut verdefs = HashMap::default();
    if let Some((mut at, _, _, count)) = find(SHT_GNU_VERDEF) {
        for _ in 0..count {
            let ndx = c.e.u16(raw, at + 4);
            let aux = c.e.u32(raw, at + 12) as usize;
            let next = c.e.u32(raw, at + 16) as usize;
            let name = cstr(raw, strtab + c.e.u32(raw, at + aux) as usize);
            verdefs.insert(ndx, name.to_owned());
            if next == 0 {
                break;
            }
            at += next;
        }
    }

    let mut exports = HashMap::default();
    let mut versioned = HashMap::default();
    let mut undefined = Vec::new();
    for i in 1..nsyms {
        let e = c.sym(raw, sym_off, i);
        let name = cstr(raw, strtab + e.name as usize);
        let (bind, ty) = (e.info >> 4, e.info & 0xf);
        let vis = e.other & 3;
        let shndx = e.shndx;
        if bind == 0 || name.is_empty() {
            continue;
        }
        if shndx == 0 {
            undefined.push(name.to_owned());
            continue;
        }
        if vis == 1 || vis == 2 {
            continue;
        }
        let vs = versym.map_or(1, |v| c.e.u16(raw, v + i * 2));
        let ver = vs & !VERSYM_HIDDEN;
        if ver == 0 {
            continue;
        }
        let x = Export {
            ver,
            ty,
            size: e.size,
        };
        if vs & VERSYM_HIDDEN == 0 {
            exports.insert(name.to_owned(), x);
        }
        if let Some(v) = verdefs.get(&ver).filter(|_| ver >= 2) {
            versioned.insert((name.to_owned(), v.clone()), x);
        }
    }
    Ok(Dso {
        path: path.to_owned(),
        soname,
        as_needed: false,
        verdefs,
        exports,
        versioned,
        undefined,
    })
}

/// The inputs of a linker script: `GROUP`, `INPUT` and `AS_NEEDED`
/// entries (`true`: as needed), in order. Output format and search
/// commands are ignored; anything else is an error.
pub fn parse_script(text: &str) -> Result<Vec<(String, bool)>, String> {
    let mut toks = Vec::new();
    let mut s = text;
    loop {
        s = s.trim_start();
        if s.is_empty() {
            break;
        }
        if let Some(rest) = s.strip_prefix("/*") {
            let end = rest.find("*/").ok_or("unterminated comment")?;
            s = &rest[end + 2..];
            continue;
        }
        let c = s.as_bytes()[0];
        if matches!(c, b'(' | b')' | b',' | b';') {
            toks.push(&s[..1]);
            s = &s[1..];
            continue;
        }
        let end = s
            .find(|ch: char| ch.is_whitespace() || "(),;".contains(ch))
            .unwrap_or(s.len());
        toks.push(&s[..end]);
        s = &s[end..];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        i += 1;
        if t == ";" {
            continue;
        }
        if toks.get(i) != Some(&"(") {
            return Err(format!("unsupported linker script: `{t}`"));
        }
        i += 1;
        let ignore = matches!(t, "OUTPUT_FORMAT" | "OUTPUT_ARCH" | "SEARCH_DIR" | "TARGET");
        if !ignore && !matches!(t, "GROUP" | "INPUT") {
            return Err(format!("unsupported linker script command {t}"));
        }
        let mut needed = false;
        while i < toks.len() {
            let a = toks[i];
            i += 1;
            match a {
                ")" if needed => needed = false,
                ")" => break,
                "," => {}
                "AS_NEEDED" if !ignore => {
                    if toks.get(i) != Some(&"(") {
                        return Err("AS_NEEDED without `(`".to_owned());
                    }
                    i += 1;
                    needed = true;
                }
                _ if ignore => {}
                _ => out.push((a.to_owned(), needed)),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_glibc_scripts() {
        let s = "/* GNU ld script */\nOUTPUT_FORMAT(elf64-x86-64)\nGROUP ( /lib/x86_64-linux-gnu/libc.so.6 \
                 /usr/lib/x86_64-linux-gnu/libc_nonshared.a  AS_NEEDED ( /lib64/ld-linux-x86-64.so.2 ) )\n";
        assert_eq!(
            parse_script(s).unwrap(),
            vec![
                ("/lib/x86_64-linux-gnu/libc.so.6".to_owned(), false),
                (
                    "/usr/lib/x86_64-linux-gnu/libc_nonshared.a".to_owned(),
                    false
                ),
                ("/lib64/ld-linux-x86-64.so.2".to_owned(), true),
            ]
        );
        assert_eq!(
            parse_script("INPUT(-lfoo)").unwrap(),
            vec![("-lfoo".to_owned(), false)]
        );
        assert!(parse_script("SECTIONS { }").is_err());
    }
}
