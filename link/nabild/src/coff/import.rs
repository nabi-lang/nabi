//! Imports from DLLs and import libraries.
//!
//! Short import members (`IMPORT_OBJECT_HEADER`) load into IAT slots and
//! thunks; module definitions (`.def`, mingw-w64 syntax) become import
//! libraries as llvm-dlltool writes them.

use crate::archive;
use crate::bytes::{cstr, put32, u16le, u32le};
use crate::coff::{Import, Link, MACHINE_AMD64, MACHINE_ARM64, MACHINE_I386};
use crate::symtab::{AtomId, Def, Reloc, Target};

const IMPORT_CODE: u8 = 0;
const IMPORT_DATA: u8 = 1;
const IMPORT_CONST: u8 = 2;

const NAME_ORDINAL: u8 = 0;
const NAME: u8 = 1;
const NAME_NOPREFIX: u8 = 2;
const NAME_UNDECORATE: u8 = 3;
const NAME_EXPORTAS: u8 = 4;

const AMD64_ADDR32NB: u16 = 3;
const AMD64_REL32: u16 = 4;
const ARM64_ADDR32NB: u16 = 2;
const ARM64_PAGEBASE_REL21: u16 = 4;
const ARM64_PAGEOFFSET_12L: u16 = 7;
const I386_DIR32: u16 = 6;
const I386_DIR32NB: u16 = 7;

/// `d` is a short import member: `Sig1` 0, `Sig2` 0xffff, version 0.
#[must_use]
pub fn is_short_import(d: &[u8]) -> bool {
    d.len() >= 20 && u16le(d, 0) == 0 && u16le(d, 2) == 0xffff && u16le(d, 4) == 0
}

/// A short import member's fields.
struct Short<'a> {
    machine: u16,
    hint: u16,
    ty: u8,
    name_type: u8,
    sym: &'a str,
    dll: &'a str,
    export_as: &'a str,
}

fn read_short(d: &[u8]) -> Result<Short<'_>, String> {
    let size = u32le(d, 12) as usize;
    if d.len() < 20 + size {
        return Err("broken import library".to_owned());
    }
    let info = d[18];
    let sym = cstr(d, 20);
    let dll = cstr(d, 21 + sym.len());
    let export_as = cstr(d, 22 + sym.len() + dll.len());
    Ok(Short {
        machine: u16le(d, 6),
        hint: u16le(d, 16),
        ty: info & 3,
        name_type: (info >> 2) & 7,
        sym,
        dll,
        export_as: if 22 + sym.len() + dll.len() < 20 + size {
            export_as
        } else {
            ""
        },
    })
}

/// The first character dropped if one of `chars`.
fn ltrim1<'a>(s: &'a str, chars: &str) -> &'a str {
    match s.chars().next() {
        Some(c) if chars.contains(c) => &s[1..],
        _ => s,
    }
}

/// The name a DLL exports for symbol `name` imported with `name_type`.
fn apply_name_type(name_type: u8, name: &str) -> String {
    match name_type {
        NAME_NOPREFIX => ltrim1(name, "?@_").to_owned(),
        NAME_UNDECORATE => {
            let n = ltrim1(name, "?@_");
            n[..n.find('@').unwrap_or(n.len())].to_owned()
        }
        _ => name.to_owned(),
    }
}

/// Load short import member `d`: an IAT slot and lookup table entry
/// (`__imp_<name>`), its hint / name entry, and for code a thunk
/// jumping through the slot (`<name>`).
pub fn load(l: &mut Link, file: &str, d: &[u8]) -> Result<(), String> {
    let s = read_short(d).map_err(|e| format!("{file}: {e}"))?;
    if s.machine != l.arch.machine() {
        return Err(format!(
            "{file}: machine type {:#x} conflicts with {}",
            s.machine,
            l.arch.name()
        ));
    }
    let name = match s.name_type {
        NAME_ORDINAL => None,
        NAME_EXPORTAS => Some(s.export_as.to_owned()),
        t => Some(apply_name_type(t, s.sym)),
    };
    let arch = l.arch;
    let rva = match arch {
        crate::coff::Arch::X86_64 => AMD64_ADDR32NB,
        crate::coff::Arch::Aarch64 => ARM64_ADDR32NB,
        crate::coff::Arch::X86 => I386_DIR32NB,
    };
    let hint_name = name.as_ref().map(|n| {
        let mut b = s.hint.to_le_bytes().to_vec();
        b.extend_from_slice(n.as_bytes());
        b.push(0);
        if b.len() % 2 == 1 {
            b.push(0);
        }
        l.synth(".idata$6", crate::coff::RDATA, 1, b, 0)
    });
    let slot = |l: &mut Link, sect: &str| {
        // By ordinal: the top bit set.
        let (b, align) = match (arch.is64(), hint_name.is_none()) {
            (true, true) => (((1u64 << 63) | u64::from(s.hint)).to_le_bytes().to_vec(), 3),
            (true, false) => (vec![0; 8], 3),
            (false, true) => (((1u32 << 31) | u32::from(s.hint)).to_le_bytes().to_vec(), 2),
            (false, false) => (vec![0; 4], 2),
        };
        let a = l.synth(sect, crate::coff::RDATA, align, b, 0);
        if let Some(h) = hint_name {
            l.atoms[a as usize]
                .relocs
                .push(reloc(0, rva, 2, false, Target::Atom(h)));
        }
        a
    };
    let ilt = slot(l, ".idata$4");
    let iat = slot(l, ".idata$5");
    l.atoms[iat as usize].keep.push(Target::Atom(ilt));
    if let Some(h) = hint_name {
        l.atoms[iat as usize].keep.push(Target::Atom(h));
    }
    let fi = u32::MAX;
    let define = |l: &mut Link, n: &str, atom: AtomId| {
        let id = l.syms.intern(n);
        if let Err(prev) = l
            .syms
            .define(id, Def::Defined { atom, off: 0 }, fi, false, false)
        {
            l.errors.push(format!(
                "duplicate symbol: {n}\n>>> defined at {}\n>>> defined by {file}",
                l.file_name(prev)
            ));
        }
    };
    define(l, &format!("__imp_{}", s.sym), iat);
    match s.ty {
        IMPORT_CODE => {
            // jmp [rip + slot] / jmp [slot] / adrp, ldr, br
            let jmp = vec![0xff, 0x25, 0, 0, 0, 0];
            let (code, align, relocs) = match arch {
                crate::coff::Arch::X86_64 => (
                    jmp,
                    1,
                    vec![reloc(2, AMD64_REL32, 2, true, Target::Atom(iat))],
                ),
                crate::coff::Arch::X86 => (
                    jmp,
                    0,
                    vec![reloc(2, I386_DIR32, 2, false, Target::Atom(iat))],
                ),
                crate::coff::Arch::Aarch64 => {
                    let mut b = Vec::new();
                    for w in [0x9000_0010u32, 0xf940_0210, 0xd61f_0200] {
                        put32(&mut b, w);
                    }
                    let r = vec![
                        reloc(0, ARM64_PAGEBASE_REL21, 2, true, Target::Atom(iat)),
                        reloc(4, ARM64_PAGEOFFSET_12L, 2, false, Target::Atom(iat)),
                    ];
                    (b, 2, r)
                }
            };
            let thunk = l.synth(".text", crate::coff::CODE, align, code, 0);
            l.atoms[thunk as usize].relocs = relocs;
            define(l, s.sym, thunk);
        }
        IMPORT_CONST => define(l, s.sym, iat),
        IMPORT_DATA => {}
        t => return Err(format!("{file}: unknown import type {t}")),
    }
    l.imports.push(Import {
        dll: s.dll.to_owned(),
        sym: s.sym.to_owned(),
        name,
        ordinal: s.hint,
        iat,
        ilt,
        hint_name,
    });
    Ok(())
}

const fn reloc(off: u32, kind: u16, len: u8, pcrel: bool, target: Target) -> Reloc {
    Reloc {
        off,
        kind,
        len,
        pcrel,
        target,
        addend: 0,
        sub: None,
    }
}

/// The symbols an archive member defines.
///
/// For an archive without a symbol table: a short import's
/// `__imp_<name>` (and `<name>` but for data), an object's external
/// definitions, commons and weak externals.
#[must_use]
pub fn member_symbols(d: &[u8]) -> Vec<String> {
    if is_short_import(d) {
        let Ok(s) = read_short(d) else {
            return Vec::new();
        };
        let mut v = vec![format!("__imp_{}", s.sym)];
        if s.ty != IMPORT_DATA {
            v.push(s.sym.to_owned());
        }
        return v;
    }
    let Ok(h) = crate::coff::object::header(d) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < h.nsyms {
        let sn = h.section(d, i);
        let (value, _, class, naux) = h.fields(d, i);
        let defined = sn > 0 || sn == -1 || (sn == 0 && value != 0);
        if (class == 2 && defined) || class == 105 {
            out.push(h.name(d, i).to_owned());
        }
        i += 1 + naux;
    }
    out
}

/// An export of a module definition.
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
struct DefExport {
    name: String,
    ext_name: String,
    symbol_name: String,
    import_name: String,
    export_as: String,
    ordinal: u16,
    noname: bool,
    data: bool,
    constant: bool,
    private: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Equal,
    EqualEqual,
    Comma,
    Eof,
}

/// Tokens of a module definition, keywords as identifiers.
fn lex(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut b = s;
    loop {
        b = b.trim_start();
        let Some(c) = b.chars().next() else { break };
        match c {
            '\0' => break,
            ';' => b = b.find('\n').map_or("", |e| &b[e..]),
            '=' => {
                if b.starts_with("==") {
                    out.push(Tok::EqualEqual);
                    b = &b[2..];
                } else {
                    out.push(Tok::Equal);
                    b = &b[1..];
                }
            }
            ',' => {
                out.push(Tok::Comma);
                b = &b[1..];
            }
            '"' => {
                let r = &b[1..];
                let e = r.find('"').unwrap_or(r.len());
                out.push(Tok::Ident(r[..e].to_owned()));
                b = r.get(e + 1..).unwrap_or("");
            }
            _ => {
                let e = b
                    .find(|c: char| "=,;\r\n \t\x0b".contains(c))
                    .unwrap_or(b.len());
                out.push(Tok::Ident(b[..e].to_owned()));
                b = &b[e..];
            }
        }
    }
    out.push(Tok::Eof);
    out
}

const KEYWORDS: &[&str] = &[
    "BASE",
    "CONSTANT",
    "DATA",
    "EXPORTS",
    "EXPORTAS",
    "HEAPSIZE",
    "LIBRARY",
    "NAME",
    "NONAME",
    "PRIVATE",
    "STACKSIZE",
    "VERSION",
];

fn is_word(t: &Tok, w: &str) -> bool {
    matches!(t, Tok::Ident(s) if s == w)
}

fn ident(t: &Tok) -> Option<&str> {
    match t {
        Tok::Ident(s) if !KEYWORDS.contains(&s.as_str()) => Some(s),
        _ => None,
    }
}

/// A name of a mingw-w64 module definition for x86 (gets an `_`) or not.
fn decorated(sym: &str) -> bool {
    sym.starts_with('@') || sym.contains("@@") || sym.starts_with('?')
}

/// Parse module definition `text` (mingw-w64 syntax): the DLL's name and
/// the exports, x86 names `_`-prefixed.
fn parse_def(text: &str, x86: bool) -> Result<(String, Vec<DefExport>), String> {
    let t = lex(text);
    let mut i = 0;
    let mut dll = String::new();
    let mut exports = Vec::new();
    while t[i] != Tok::Eof {
        if is_word(&t[i], "LIBRARY") || is_word(&t[i], "NAME") {
            let lib = is_word(&t[i], "LIBRARY");
            i += 1;
            let n = ident(&t[i]).ok_or("LIBRARY: a name expected")?.to_owned();
            i += 1;
            if is_word(&t[i], "BASE") {
                return Err("LIBRARY ... BASE= is not supported".to_owned());
            }
            dll = if std::path::Path::new(&n).extension().is_some() {
                n
            } else if lib {
                format!("{n}.dll")
            } else {
                format!("{n}.exe")
            };
        } else if is_word(&t[i], "EXPORTS") {
            i += 1;
            while let Some(n) = ident(&t[i]) {
                let mut e = DefExport {
                    name: n.to_owned(),
                    ..DefExport::default()
                };
                i += 1;
                if t[i] == Tok::Equal {
                    let to = ident(&t[i + 1]).ok_or("identifier expected after =")?;
                    e.ext_name = std::mem::replace(&mut e.name, to.to_owned());
                    i += 2;
                }
                if x86 {
                    if !decorated(&e.name) && (e.ext_name.is_empty() || !e.name.contains('.')) {
                        e.name.insert(0, '_');
                    }
                    if !e.ext_name.is_empty() && !decorated(&e.ext_name) {
                        e.ext_name.insert(0, '_');
                    }
                }
                loop {
                    match &t[i] {
                        Tok::Ident(s) if s.starts_with('@') => {
                            if s == "@" {
                                i += 1;
                                e.ordinal = ident(&t[i]).and_then(|v| v.parse().ok()).unwrap_or(0);
                            } else if let Ok(v) = s[1..].parse() {
                                e.ordinal = v;
                            } else {
                                // The next export, fastcall decorated.
                                break;
                            }
                            i += 1;
                            if is_word(&t[i], "NONAME") {
                                e.noname = true;
                                i += 1;
                            }
                        }
                        Tok::Ident(s) if s == "DATA" => {
                            e.data = true;
                            i += 1;
                        }
                        Tok::Ident(s) if s == "CONSTANT" => {
                            e.constant = true;
                            i += 1;
                        }
                        Tok::Ident(s) if s == "PRIVATE" => {
                            e.private = true;
                            i += 1;
                        }
                        Tok::EqualEqual => {
                            if let Tok::Ident(s) = &t[i + 1] {
                                e.import_name.clone_from(s);
                            }
                            i += 2;
                        }
                        Tok::Ident(s) if s == "EXPORTAS" => {
                            match &t[i + 1] {
                                Tok::Ident(s) => e.export_as.clone_from(s),
                                _ => return Err("EXPORTAS: identifier expected".to_owned()),
                            }
                            i += 2;
                            break;
                        }
                        _ => break,
                    }
                }
                exports.push(e);
            }
        } else {
            return Err(format!("unknown directive: {:?}", t[i]));
        }
    }
    if dll.is_empty() {
        return Err("no LIBRARY".to_owned());
    }
    Ok((dll, exports))
}

/// A COFF header; `chars` 32-bit on x86 when `bits`.
fn coff_header(b: &mut Vec<u8>, machine: u16, nsects: u16, symoff: u32, nsyms: u32, bits: bool) {
    b.extend_from_slice(&machine.to_le_bytes());
    b.extend_from_slice(&nsects.to_le_bytes());
    put32(b, 0);
    put32(b, symoff);
    put32(b, nsyms);
    b.extend_from_slice(&0u16.to_le_bytes());
    let chars: u16 = if bits && machine == MACHINE_I386 {
        0x100
    } else {
        0
    };
    b.extend_from_slice(&chars.to_le_bytes());
}

fn sect_header(
    b: &mut Vec<u8>,
    name: &str,
    size: u32,
    ptr: u32,
    relptr: u32,
    nrel: u16,
    chars: u32,
) {
    let mut n = [0u8; 8];
    n[..name.len()].copy_from_slice(name.as_bytes());
    b.extend_from_slice(&n);
    put32(b, 0);
    put32(b, 0);
    put32(b, size);
    put32(b, ptr);
    put32(b, relptr);
    put32(b, 0);
    b.extend_from_slice(&nrel.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    put32(b, chars);
}

/// A symbol record: inline name, or an offset into the string table.
fn symbol(b: &mut Vec<u8>, name: Result<&str, u32>, value: u32, sect: i16, class: u8, naux: u8) {
    match name {
        Ok(n) => {
            let mut raw = [0u8; 8];
            raw[..n.len()].copy_from_slice(n.as_bytes());
            b.extend_from_slice(&raw);
        }
        Err(off) => {
            put32(b, 0);
            put32(b, off);
        }
    }
    put32(b, value);
    b.extend_from_slice(&sect.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.push(class);
    b.push(naux);
}

fn string_table(b: &mut Vec<u8>, strs: &[&str]) {
    let len: usize = 4 + strs.iter().map(|s| s.len() + 1).sum::<usize>();
    put32(b, len as u32);
    for s in strs {
        b.extend_from_slice(s.as_bytes());
        b.push(0);
    }
}

const ALIGN_2: u32 = 0x0020_0000;
const ALIGN_4: u32 = 0x0030_0000;
const ALIGN_8: u32 = 0x0040_0000;
const DATA_RW: u32 = 0x40 | 0x4000_0000 | 0x8000_0000;

/// `__IMPORT_DESCRIPTOR_<lib>`: the DLL's import directory entry and
/// name, referring to the terminators.
fn import_descriptor(machine: u16, dll: &str, lib: &str) -> Vec<u8> {
    let rva = img_rel(machine);
    let mut b = Vec::new();
    let data = 20 + 2 * 40;
    let rel = data + 20;
    let name = rel + 3 * 10;
    let symoff = name + dll.len() + 1;
    coff_header(&mut b, machine, 2, symoff as u32, 7, true);
    sect_header(
        &mut b,
        ".idata$2",
        20,
        data as u32,
        rel as u32,
        3,
        ALIGN_4 | DATA_RW,
    );
    sect_header(
        &mut b,
        ".idata$6",
        dll.len() as u32 + 1,
        name as u32,
        0,
        0,
        ALIGN_2 | DATA_RW,
    );
    b.resize(b.len() + 20, 0);
    for (off, sym) in [(12u32, 2u32), (0, 3), (16, 4)] {
        put32(&mut b, off);
        put32(&mut b, sym);
        b.extend_from_slice(&rva.to_le_bytes());
    }
    b.extend_from_slice(dll.as_bytes());
    b.push(0);
    let desc = format!("__IMPORT_DESCRIPTOR_{lib}");
    let thunk = format!("\x7f{lib}_NULL_THUNK_DATA");
    symbol(&mut b, Err(4), 0, 1, 2, 0);
    symbol(&mut b, Ok(".idata$2"), 0, 1, 104, 0);
    symbol(&mut b, Ok(".idata$6"), 0, 2, 3, 0);
    symbol(&mut b, Ok(".idata$4"), 0, 0, 104, 0);
    symbol(&mut b, Ok(".idata$5"), 0, 0, 104, 0);
    symbol(&mut b, Err(4 + desc.len() as u32 + 1), 0, 0, 2, 0);
    symbol(
        &mut b,
        Err(4 + desc.len() as u32 + 1 + NULL_DESCRIPTOR.len() as u32 + 1),
        0,
        0,
        2,
        0,
    );
    string_table(&mut b, &[&desc, NULL_DESCRIPTOR, &thunk]);
    b
}

const NULL_DESCRIPTOR: &str = "__NULL_IMPORT_DESCRIPTOR";

/// `__NULL_IMPORT_DESCRIPTOR`: the directory's terminator.
fn null_import_descriptor(machine: u16) -> Vec<u8> {
    let mut b = Vec::new();
    coff_header(&mut b, machine, 1, 20 + 40 + 20, 1, true);
    sect_header(&mut b, ".idata$3", 20, 60, 0, 0, ALIGN_4 | DATA_RW);
    b.resize(b.len() + 20, 0);
    symbol(&mut b, Err(4), 0, 1, 2, 0);
    string_table(&mut b, &[NULL_DESCRIPTOR]);
    b
}

/// `\x7f<lib>_NULL_THUNK_DATA`: the IAT and lookup table terminators.
fn null_thunk(machine: u16, lib: &str) -> Vec<u8> {
    let w: u32 = if machine == MACHINE_I386 { 4 } else { 8 };
    let align = if w == 8 { ALIGN_8 } else { ALIGN_4 };
    let mut b = Vec::new();
    coff_header(&mut b, machine, 2, 20 + 80 + 2 * w, 1, true);
    sect_header(&mut b, ".idata$5", w, 100, 0, 0, align | DATA_RW);
    sect_header(&mut b, ".idata$4", w, 100 + w, 0, 0, align | DATA_RW);
    b.resize(b.len() + 2 * w as usize, 0);
    symbol(&mut b, Err(4), 0, 1, 2, 0);
    string_table(&mut b, &[&format!("\x7f{lib}_NULL_THUNK_DATA")]);
    b
}

/// A short import member.
fn short_import(
    machine: u16,
    sym: &str,
    dll: &str,
    ordinal: u16,
    ty: u8,
    name_type: u8,
    export: &str,
) -> Vec<u8> {
    let mut size = sym.len() + dll.len() + 2;
    if !export.is_empty() {
        size += export.len() + 1;
    }
    let mut b = Vec::with_capacity(20 + size);
    b.extend_from_slice(&[0, 0, 0xff, 0xff, 0, 0]);
    b.extend_from_slice(&machine.to_le_bytes());
    put32(&mut b, 0);
    put32(&mut b, size as u32);
    b.extend_from_slice(&ordinal.to_le_bytes());
    b.extend_from_slice(&u16::from((name_type << 2) | ty).to_le_bytes());
    b.extend_from_slice(sym.as_bytes());
    b.push(0);
    b.extend_from_slice(dll.as_bytes());
    b.push(0);
    if !export.is_empty() {
        b.extend_from_slice(export.as_bytes());
        b.push(0);
    }
    b
}

/// An object aliasing `weak` (`__imp_`-prefixed with `imp`) to `sym`.
fn weak_external(machine: u16, sym: &str, weak: &str, imp: bool) -> Vec<u8> {
    let p = if imp { "__imp_" } else { "" };
    let (target, alias) = (format!("{p}{sym}"), format!("{p}{weak}"));
    let mut b = Vec::new();
    coff_header(&mut b, machine, 1, 60, 5, false);
    sect_header(&mut b, ".drectve", 0, 0, 0, 0, 0x200 | 0x800);
    symbol(&mut b, Ok("@comp.id"), 0, -1, 3, 0);
    symbol(&mut b, Ok("@feat.00"), 0, -1, 3, 0);
    symbol(&mut b, Err(4), 0, 0, 2, 0);
    symbol(&mut b, Err(4 + target.len() as u32 + 1), 0, 0, 105, 1);
    let mut aux = [0u8; 18];
    aux[0] = 2;
    aux[4] = 3;
    b.extend_from_slice(&aux);
    string_table(&mut b, &[&target, &alias]);
    b
}

const fn img_rel(machine: u16) -> u16 {
    match machine {
        MACHINE_AMD64 => AMD64_ADDR32NB,
        MACHINE_ARM64 => ARM64_ADDR32NB,
        // IMAGE_REL_I386_DIR32NB
        _ => 7,
    }
}

/// `__imp_` + name (and name for code) of a short import, x86's
/// `_`-prefixed names included.
fn import_symbols(sym: &str, ty: u8) -> Vec<String> {
    let mut v = vec![format!("__imp_{sym}")];
    if ty != IMPORT_DATA {
        v.push(sym.to_owned());
    }
    v
}

/// The import library of module definition `def` (mingw-w64 syntax) for
/// COFF `machine`, as `llvm-dlltool -k` writes it: x86 stdcall names are
/// decorated and imported undecorated.
pub fn import_library(machine: u16, def: &str) -> Result<Vec<u8>, String> {
    let x86 = machine == MACHINE_I386;
    let (dll, mut exports) = parse_def(def, x86)?;
    if x86 {
        for e in &mut exports {
            if !e.import_name.is_empty() || e.name.starts_with('?') {
                continue;
            }
            e.symbol_name.clone_from(&e.name);
            e.name.truncate(
                e.name[1.min(e.name.len())..]
                    .find('@')
                    .map_or(e.name.len(), |p| p + 1),
            );
        }
    }
    let lib = std::path::Path::new(&dll)
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let mut members: Vec<(String, Vec<u8>)> = vec![
        (dll.clone(), import_descriptor(machine, &dll, &lib)),
        (dll.clone(), null_import_descriptor(machine)),
        (dll.clone(), null_thunk(machine, &lib)),
    ];
    let mut syms: Vec<Vec<String>> = vec![
        vec![format!("__IMPORT_DESCRIPTOR_{lib}")],
        vec![NULL_DESCRIPTOR.to_owned()],
        vec![format!("\x7f{lib}_NULL_THUNK_DATA")],
    ];
    let mut regular: nabic_core::HashMap<String, String> = nabic_core::HashMap::default();
    let mut renames: Vec<(String, u8, DefExport)> = Vec::new();
    for e in &exports {
        if e.private {
            continue;
        }
        let ty = if e.constant {
            IMPORT_CONST
        } else if e.data {
            IMPORT_DATA
        } else {
            IMPORT_CODE
        };
        let symbol_name = if e.symbol_name.is_empty() {
            &e.name
        } else {
            &e.symbol_name
        };
        let name = if e.ext_name.is_empty() {
            symbol_name.clone()
        } else {
            replace(symbol_name, &e.name, &e.ext_name)?
        };
        let mut export_name = String::new();
        let name_type = if e.noname {
            NAME_ORDINAL
        } else if !e.export_as.is_empty() {
            export_name.clone_from(&e.export_as);
            NAME_EXPORTAS
        } else if !e.import_name.is_empty() {
            if x86 && apply_name_type(NAME_UNDECORATE, &name) == e.import_name {
                NAME_UNDECORATE
            } else if x86 && apply_name_type(NAME_NOPREFIX, &name) == e.import_name {
                NAME_NOPREFIX
            } else if name == e.import_name {
                NAME
            } else {
                renames.push((name, ty, e.clone()));
                continue;
            }
        } else {
            name_type(symbol_name, &e.name, machine)
        };
        regular.insert(apply_name_type(name_type, &name), name.clone());
        syms.push(import_symbols(&name, ty));
        members.push((
            dll.clone(),
            short_import(machine, &name, &dll, e.ordinal, ty, name_type, &export_name),
        ));
    }
    for (name, ty, e) in renames {
        if let Some(sym) = regular.get(&e.import_name) {
            if ty == IMPORT_CODE {
                members.push((dll.clone(), weak_external(machine, sym, &name, false)));
                syms.push(vec![name.clone()]);
            }
            members.push((dll.clone(), weak_external(machine, sym, &name, true)));
            syms.push(vec![format!("__imp_{name}")]);
        } else {
            syms.push(import_symbols(&name, ty));
            members.push((
                dll.clone(),
                short_import(
                    machine,
                    &name,
                    &dll,
                    e.ordinal,
                    ty,
                    NAME_EXPORTAS,
                    &e.import_name,
                ),
            ));
        }
    }
    Ok(archive::write_with(archive::Kind::Coff, &members, &syms))
}

/// How a DLL exports symbol `sym`, listed as `ext` (mingw: x86 stdcall
/// names lose the `_` and the `@<n>`).
fn name_type(sym: &str, ext: &str, machine: u16) -> u8 {
    if sym != ext {
        NAME_UNDECORATE
    } else if machine == MACHINE_I386 && sym.starts_with('_') {
        NAME_NOPREFIX
    } else {
        NAME
    }
}

/// `s` with `from` replaced by `to` (or both without their `_`).
fn replace(s: &str, from: &str, to: &str) -> Result<String, String> {
    if let Some(p) = s.find(from) {
        return Ok(format!("{}{to}{}", &s[..p], &s[p + from.len()..]));
    }
    if let (Some(f), Some(t)) = (from.strip_prefix('_'), to.strip_prefix('_')) {
        if let Some(p) = s.find(f) {
            return Ok(format!("{}{t}{}", &s[..p], &s[p + f.len()..]));
        }
    }
    Err(format!("{s}: replacing '{from}' with '{to}' failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEF: &str = "; comment\nLIBRARY \"KERNEL32.dll\"\nEXPORTS\nExitProcess\n\
        NlsAnsiCodePage DATA\nord_1000 @1000\nchdir == _chdir\n_chdir\nhidden == nowhere\n";

    #[test]
    fn writes_what_dlltool_writes() {
        let lib = import_library(MACHINE_AMD64, DEF).unwrap();
        let a = archive::parse(&lib).unwrap();
        // Descriptor, terminators, 4 short imports, the alias pair of
        // chdir, hidden by EXPORTAS.
        assert_eq!(a.members.len(), 3 + 4 + 2 + 1);
        assert!(a.members.iter().all(|m| m.name == "KERNEL32.dll"));
        let find = |n: &str| a.symbols.iter().find(|(s, _)| s == n).map(|(_, i)| *i);
        let exit = find("ExitProcess").unwrap();
        assert_eq!(find("__imp_ExitProcess"), Some(exit));
        assert_eq!(find("NlsAnsiCodePage"), None);
        let s = read_short(a.members[exit].data).unwrap();
        assert_eq!(
            (s.sym, s.dll, s.ty, s.name_type),
            ("ExitProcess", "KERNEL32.dll", IMPORT_CODE, NAME)
        );
        let o = read_short(a.members[find("ord_1000").unwrap()].data).unwrap();
        assert_eq!((o.hint, o.name_type), (1000, NAME));
        assert!(find("chdir").is_some() && find("__imp_chdir").is_some());
        let h = read_short(a.members[find("hidden").unwrap()].data).unwrap();
        assert_eq!((h.name_type, h.export_as), (NAME_EXPORTAS, "nowhere"));
        assert!(find("__IMPORT_DESCRIPTOR_KERNEL32").is_some());
    }

    #[test]
    fn x86_names_are_decorated() {
        let def = "LIBRARY \"KERNEL32.dll\"\nEXPORTS\nAddAtomA@4\n_lseek\nfree\n";
        let lib = import_library(MACHINE_I386, def).unwrap();
        let a = archive::parse(&lib).unwrap();
        let get = |n: &str| {
            let i = a.symbols.iter().find(|(s, _)| s == n).unwrap().1;
            let s = read_short(a.members[i].data).unwrap();
            (s.name_type, apply_name_type(s.name_type, s.sym))
        };
        assert_eq!(get("_AddAtomA@4"), (NAME_UNDECORATE, "AddAtomA".to_owned()));
        assert_eq!(get("__lseek"), (NAME_NOPREFIX, "_lseek".to_owned()));
        assert_eq!(get("__imp__free"), (NAME_NOPREFIX, "free".to_owned()));
    }
}
