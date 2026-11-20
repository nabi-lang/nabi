//! `ar` archives: the reader (BSD and GNU symbol tables, long names) and
//! the writer (Darwin, GNU and COFF flavours), with the defined symbols of
//! Mach-O, ELF and COFF members for the symbol table.

use crate::bytes::{u16le, u32le, u64le};

pub const MAGIC: &[u8; 8] = b"!<arch>\n";
const HEADER: usize = 60;

/// One member: its name, where its header starts, its bytes.
pub struct Member<'a> {
    pub name: String,
    pub header: usize,
    pub data: &'a [u8],
    pub mtime: u64,
}

pub struct Archive<'a> {
    pub members: Vec<Member<'a>>,
    /// The symbol table: name and member index. Empty if the archive has
    /// none (the caller then scans the members).
    pub symbols: Vec<(String, usize)>,
}

fn field(b: &[u8]) -> &str {
    std::str::from_utf8(b)
        .unwrap_or("")
        .trim_end_matches([' ', '\0'])
}

fn be32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).map_or(0, |s| {
        u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    })
}

/// Parse `b`, which starts with [`MAGIC`].
pub fn parse(b: &[u8]) -> Result<Archive<'_>, String> {
    if !b.starts_with(MAGIC) {
        return Err("not an archive".to_owned());
    }
    let mut members = Vec::new();
    // Symbol tables refer to members by header offset.
    let mut raw_syms: Vec<(String, u64)> = Vec::new();
    let mut long_names: &[u8] = &[];
    let mut seen_gnu_symtab = false;
    let mut at = 8;
    while at + HEADER <= b.len() {
        let h = &b[at..at + HEADER];
        if &h[58..60] != b"`\n" {
            return Err(format!("bad member header at offset {at}"));
        }
        let size: usize = field(&h[48..58])
            .parse()
            .map_err(|_| format!("bad member size at offset {at}"))?;
        let mtime = field(&h[16..28]).parse().unwrap_or(0);
        let start = at + HEADER;
        let end = start
            .checked_add(size)
            .filter(|&e| e <= b.len())
            .ok_or_else(|| format!("member at offset {at} runs past the end"))?;
        let raw = field(&h[..16]);
        let mut data = &b[start..end];
        let name = if let Some(n) = raw.strip_prefix("#1/") {
            let n: usize = n.parse().map_err(|_| "bad BSD name length".to_owned())?;
            let n = n.min(data.len());
            let name = field(&data[..n]).to_owned();
            data = &data[n..];
            name
        } else if raw == "/" || raw == "/SYM64/" {
            // The second `/` of a COFF archive repeats the first.
            if !seen_gnu_symtab {
                let wide = raw == "/SYM64/";
                let (count, mut p) = if wide {
                    (be64(data, 0) as usize, 8)
                } else {
                    (be32(data, 0) as usize, 4)
                };
                let w = if wide { 8 } else { 4 };
                if count
                    .checked_mul(w)
                    .and_then(|n| n.checked_add(p))
                    .is_none_or(|e| e > data.len())
                {
                    return Err("archive symbol table runs past its member".to_owned());
                }
                let mut s = p + count * w;
                for _ in 0..count {
                    let off = if wide {
                        be64(data, p)
                    } else {
                        u64::from(be32(data, p))
                    };
                    p += w;
                    let name = crate::bytes::cstr(data, s).to_owned();
                    s += name.len() + 1;
                    raw_syms.push((name, off));
                }
                seen_gnu_symtab = true;
            }
            at = end + (end & 1);
            continue;
        } else if raw == "//" {
            long_names = data;
            at = end + (end & 1);
            continue;
        } else if let Some(off) = raw.strip_prefix('/').and_then(|o| o.parse::<usize>().ok()) {
            let s = long_names.get(off..).unwrap_or_default();
            let e = s
                .iter()
                .position(|&c| c == b'\n' || c == 0)
                .unwrap_or(s.len());
            String::from_utf8_lossy(&s[..e])
                .trim_end_matches('/')
                .to_owned()
        } else {
            raw.trim_end_matches('/').to_owned()
        };
        if name.starts_with("__.SYMDEF") {
            bsd_symtab(data, name.contains("_64"), &mut raw_syms);
        } else {
            members.push(Member {
                name,
                header: at,
                data,
                mtime,
            });
        }
        at = end + (end & 1);
    }
    let mut symbols = Vec::with_capacity(raw_syms.len());
    for (name, off) in raw_syms {
        if let Ok(i) = members.binary_search_by_key(&(off as usize), |m| m.header) {
            symbols.push((name, i));
        }
    }
    Ok(Archive { members, symbols })
}

fn bsd_symtab(d: &[u8], wide: bool, out: &mut Vec<(String, u64)>) {
    let (w, n) = if wide {
        (8, u64le(d, 0) as usize)
    } else {
        (4, u32le(d, 0) as usize)
    };
    // A size past the member is not a table.
    let n = n.min(d.len());
    let entries = w;
    let strs = entries + n + w;
    for k in 0..n / (2 * w) {
        let e = entries + k * 2 * w;
        let (strx, off) = if wide {
            (u64le(d, e) as usize, u64le(d, e + 8))
        } else {
            (u32le(d, e) as usize, u64::from(u32le(d, e + 4)))
        };
        out.push((
            crate::bytes::cstr(d, strs.saturating_add(strx)).to_owned(),
            off,
        ));
    }
}

/// Archive flavour: what reads it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// BSD names and `__.SYMDEF`, members 8-aligned (ld64, lld).
    Darwin,
    /// GNU names and `/` symbol table.
    Gnu,
    /// GNU plus the sorted second linker member of MSVC archives.
    Coff,
}

fn header(out: &mut Vec<u8>, name: &str, size: usize) {
    let mut h = format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{size:<10}`\n", 0, 0, 0, 644);
    if name == "/" || name == "//" {
        // Symbol and name tables carry no mode.
        h.replace_range(40..48, "0       ");
    }
    out.extend_from_slice(h.as_bytes());
}

/// An archive of `members` (file name, bytes) with a symbol table of the
/// symbols each defines.
#[must_use]
pub fn write(kind: Kind, members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let syms: Vec<Vec<String>> = members.iter().map(|(_, d)| defined_symbols(d)).collect();
    write_with(kind, members, &syms)
}

/// An archive of `members` whose symbol table lists `syms[i]` for
/// member `i`.
#[must_use]
pub fn write_with(kind: Kind, members: &[(String, Vec<u8>)], syms: &[Vec<String>]) -> Vec<u8> {
    match kind {
        Kind::Darwin => write_darwin(members, syms),
        Kind::Gnu | Kind::Coff => write_gnu(members, syms, kind == Kind::Coff),
    }
}

fn write_darwin(members: &[(String, Vec<u8>)], syms: &[Vec<String>]) -> Vec<u8> {
    // `#1/<n>` names, padded with NULs so member data starts 8-aligned.
    let name_field = |pos: usize, name: &str| {
        let n = name.len() + 1;
        n + (8 - (pos + HEADER + n) % 8) % 8
    };
    let mut strtab = Vec::new();
    let mut entries: Vec<(u32, usize)> = Vec::new();
    for (i, s) in syms.iter().enumerate() {
        for name in s {
            entries.push((strtab.len() as u32, i));
            strtab.extend_from_slice(name.as_bytes());
            strtab.push(0);
        }
    }
    while strtab.len() % 8 != 0 {
        strtab.push(0);
    }
    let symtab_body = 4 + 8 * entries.len() + 4 + strtab.len();
    let symdef = "__.SYMDEF";
    let sym_name = name_field(8, symdef);
    let mut pos = 8 + HEADER + sym_name + symtab_body;
    pos += (8 - pos % 8) % 8;
    let mut offsets = Vec::with_capacity(members.len());
    for (name, data) in members {
        offsets.push(pos);
        let n = name_field(pos, name);
        pos += HEADER + n + data.len().next_multiple_of(8);
    }
    let mut out = Vec::with_capacity(pos);
    out.extend_from_slice(MAGIC);
    let body_pad = (8 - (8 + HEADER + sym_name + symtab_body) % 8) % 8;
    header(
        &mut out,
        &format!("#1/{sym_name}"),
        sym_name + symtab_body + body_pad,
    );
    out.extend_from_slice(symdef.as_bytes());
    out.resize(out.len() + sym_name - symdef.len(), 0);
    crate::bytes::put32(&mut out, 8 * entries.len() as u32);
    for &(strx, m) in &entries {
        crate::bytes::put32(&mut out, strx);
        crate::bytes::put32(&mut out, offsets[m] as u32);
    }
    crate::bytes::put32(&mut out, strtab.len() as u32);
    out.extend_from_slice(&strtab);
    out.resize(out.len() + body_pad, 0);
    for (k, (name, data)) in members.iter().enumerate() {
        debug_assert_eq!(out.len(), offsets[k]);
        let n = name_field(out.len(), name);
        let padded = data.len().next_multiple_of(8);
        header(&mut out, &format!("#1/{n}"), n + padded);
        out.extend_from_slice(name.as_bytes());
        out.resize(out.len() + n - name.len(), 0);
        out.extend_from_slice(data);
        out.resize(out.len() + padded - data.len(), 0);
    }
    out
}

fn write_gnu(members: &[(String, Vec<u8>)], syms: &[Vec<String>], coff: bool) -> Vec<u8> {
    let even = |n: usize| n + (n & 1);
    // Long names.
    let mut longnames = Vec::new();
    // COFF (as llvm-lib): a name once, symbol tables padded inside.
    let mut seen: nabic_core::HashMap<&str, usize> = nabic_core::HashMap::default();
    let names: Vec<String> = members
        .iter()
        .map(|(n, _)| {
            if n.len() <= 15 && !n.contains('/') && !n.contains(' ') {
                format!("{n}/")
            } else if let Some(off) = seen.get(n.as_str()).filter(|_| coff) {
                format!("/{off}")
            } else {
                let off = longnames.len();
                seen.insert(n, off);
                longnames.extend_from_slice(n.as_bytes());
                longnames.extend_from_slice(if coff { b"\0" } else { b"/\n" });
                format!("/{off}")
            }
        })
        .collect();
    if coff && longnames.len() % 2 == 1 {
        longnames.push(b'\n');
    }
    let all: Vec<(usize, &String)> = syms
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.iter().map(move |n| (i, n)))
        .collect();
    let mut first_len = 4 + 4 * all.len() + all.iter().map(|(_, n)| n.len() + 1).sum::<usize>();
    if coff {
        first_len = even(first_len);
    }
    let mut sorted = all.clone();
    sorted.sort_by(|a, b| a.1.cmp(b.1));
    let second_len = if coff {
        even(
            4 + 4 * members.len()
                + 4
                + 2 * all.len()
                + all.iter().map(|(_, n)| n.len() + 1).sum::<usize>(),
        )
    } else {
        0
    };
    let mut pos = 8 + HEADER + even(first_len);
    if coff {
        pos += HEADER + even(second_len);
    }
    if !longnames.is_empty() {
        pos += HEADER + even(longnames.len());
    }
    let mut offsets = Vec::with_capacity(members.len());
    for (_, data) in members {
        offsets.push(pos);
        pos += HEADER + even(data.len());
    }
    let mut out = Vec::with_capacity(pos);
    out.extend_from_slice(MAGIC);
    header(&mut out, "/", first_len);
    out.extend_from_slice(&(all.len() as u32).to_be_bytes());
    for (m, _) in &all {
        out.extend_from_slice(&(offsets[*m] as u32).to_be_bytes());
    }
    for (_, n) in &all {
        out.extend_from_slice(n.as_bytes());
        out.push(0);
    }
    if out.len() & 1 == 1 {
        out.push(if coff { 0 } else { b'\n' });
    }
    if coff {
        header(&mut out, "/", second_len);
        crate::bytes::put32(&mut out, members.len() as u32);
        for o in &offsets {
            crate::bytes::put32(&mut out, *o as u32);
        }
        crate::bytes::put32(&mut out, sorted.len() as u32);
        for (m, _) in &sorted {
            out.extend_from_slice(&(*m as u16 + 1).to_le_bytes());
        }
        for (_, n) in &sorted {
            out.extend_from_slice(n.as_bytes());
            out.push(0);
        }
        if out.len() & 1 == 1 {
            out.push(0);
        }
    }
    if !longnames.is_empty() {
        if coff {
            // Blank fields but the size, as llvm-lib and lib.exe write.
            let h = format!("{:<48}{:<10}`\n", "//", longnames.len());
            out.extend_from_slice(h.as_bytes());
        } else {
            header(&mut out, "//", longnames.len());
        }
        out.extend_from_slice(&longnames);
        if out.len() & 1 == 1 {
            out.push(b'\n');
        }
    }
    for (k, (_, data)) in members.iter().enumerate() {
        debug_assert_eq!(out.len(), offsets[k]);
        header(&mut out, &names[k], data.len());
        out.extend_from_slice(data);
        if out.len() & 1 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// The external symbols object `d` defines (commons included), for an
/// archive symbol table. Unknown formats define nothing.
#[must_use]
pub fn defined_symbols(d: &[u8]) -> Vec<String> {
    match u32le(d, 0) {
        0xfeed_facf => macho_symbols(d, true),
        0xfeed_face => macho_symbols(d, false),
        _ if d.starts_with(b"\x7fELF") => elf_symbols(d),
        _ if coff_machine(u16le(d, 0)) && u16le(d, 16) == 0 => coff_symbols(d),
        _ => Vec::new(),
    }
}

fn macho_symbols(d: &[u8], wide: bool) -> Vec<String> {
    let ncmds = u32le(d, 16);
    let mut at = if wide { 32 } else { 28 };
    let mut out = Vec::new();
    for _ in 0..ncmds {
        let (cmd, size) = (u32le(d, at), u32le(d, at + 4) as usize);
        if cmd == 0x2 {
            let (symoff, nsyms) = (u32le(d, at + 8) as usize, u32le(d, at + 12) as usize);
            let stroff = u32le(d, at + 16) as usize;
            let esize = if wide { 16 } else { 12 };
            for k in 0..nsyms {
                let e = symoff + k * esize;
                if e >= d.len() {
                    break;
                }
                let ty = d.get(e + 4).copied().unwrap_or(0);
                let value = if wide {
                    u64le(d, e + 8)
                } else {
                    u64::from(u32le(d, e + 8))
                };
                // Not a stab, external, and defined (or common).
                let defined =
                    matches!(ty & 0x0e, 0xe | 0x2 | 0xa) || (ty & 0x0e == 0 && value != 0);
                if ty & 0xe0 == 0 && ty & 1 == 1 && defined {
                    out.push(crate::bytes::cstr(d, stroff + u32le(d, e) as usize).to_owned());
                }
            }
        }
        if size == 0 {
            break;
        }
        at += size;
    }
    out
}

fn elf_symbols(d: &[u8]) -> Vec<String> {
    let wide = d.get(4) == Some(&2);
    let be = d.get(5) == Some(&2);
    let r16 = |at: usize| {
        let s = d.get(at..at + 2).unwrap_or(&[0, 0]);
        if be {
            u16::from_be_bytes([s[0], s[1]])
        } else {
            u16::from_le_bytes([s[0], s[1]])
        }
    };
    let r32 = |at: usize| if be { be32(d, at) } else { u32le(d, at) };
    let r64 = |at: usize| if be { be64(d, at) } else { u64le(d, at) };
    let word = |at: usize| if wide { r64(at) } else { u64::from(r32(at)) };
    let (shoff, shentsize, shnum) = if wide {
        (r64(0x28) as usize, r16(0x3a) as usize, r16(0x3c) as usize)
    } else {
        (r32(0x20) as usize, r16(0x2e) as usize, r16(0x30) as usize)
    };
    let sh = |i: usize| shoff + i * shentsize;
    // (type, offset, size, link, entsize)
    let sect = |i: usize| {
        let s = sh(i);
        if wide {
            (
                r32(s + 4),
                r64(s + 0x18) as usize,
                r64(s + 0x20) as usize,
                r32(s + 0x28) as usize,
                r64(s + 0x38) as usize,
            )
        } else {
            (
                r32(s + 4),
                r32(s + 0x10) as usize,
                r32(s + 0x14) as usize,
                r32(s + 0x18) as usize,
                r32(s + 0x24) as usize,
            )
        }
    };
    let mut out = Vec::new();
    for i in 0..shnum {
        let (ty, off, size, link, entsize) = sect(i);
        if ty != 2 || entsize == 0 {
            continue;
        }
        let strs = sect(link).1;
        for k in 1..size / entsize {
            let e = off + k * entsize;
            let (name, info, shndx) = if wide {
                (r32(e), d.get(e + 4).copied().unwrap_or(0), r16(e + 6))
            } else {
                (r32(e), d.get(e + 12).copied().unwrap_or(0), r16(e + 14))
            };
            let _ = word;
            let bind = info >> 4;
            let stype = info & 0xf;
            if matches!(bind, 1 | 2 | 10) && shndx != 0 && !matches!(stype, 3 | 4) {
                out.push(crate::bytes::cstr(d, strs + name as usize).to_owned());
            }
        }
    }
    out
}

const fn coff_machine(m: u16) -> bool {
    matches!(m, 0x8664 | 0xaa64 | 0x14c | 0x1c4 | 0xa641)
}

fn coff_symbols(d: &[u8]) -> Vec<String> {
    let symoff = u32le(d, 8) as usize;
    let nsyms = u32le(d, 12) as usize;
    let strs = symoff + nsyms * 18;
    let mut out = Vec::new();
    let mut k = 0;
    while k < nsyms {
        let e = symoff + k * 18;
        let section = u16le(d, e + 12) as i16;
        let value = u32le(d, e + 8);
        let class = d.get(e + 16).copied().unwrap_or(0);
        let aux = d.get(e + 17).copied().unwrap_or(0) as usize;
        if class == 2 && (section > 0 || (section == 0 && value != 0) || section == -1) {
            let name = if u32le(d, e) == 0 {
                crate::bytes::cstr(d, strs + u32le(d, e + 4) as usize).to_owned()
            } else {
                let raw = d.get(e..e + 8).unwrap_or_default();
                let n = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                String::from_utf8_lossy(&raw[..n]).into_owned()
            };
            out.push(name);
        }
        k += 1 + aux;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal Mach-O object whose symbol table defines `names`.
    fn macho(names: &[&str]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [0xfeed_facf_u32, 0x0100_000c, 0, 1, 1, 24, 0, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let symoff = 32 + 24;
        let stroff = symoff + 16 * names.len();
        for v in [
            2u32,
            24,
            symoff as u32,
            names.len() as u32,
            stroff as u32,
            0,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let mut strs = vec![0u8];
        for n in names {
            b.extend_from_slice(&(strs.len() as u32).to_le_bytes());
            b.extend_from_slice(&[0x0f, 1, 0, 0]);
            b.extend_from_slice(&0u64.to_le_bytes());
            strs.extend_from_slice(n.as_bytes());
            strs.push(0);
        }
        b.extend_from_slice(&strs);
        b
    }

    #[test]
    fn round_trips_every_kind() {
        let members = vec![
            ("a.o".to_owned(), macho(&["_alpha", "_beta"])),
            ("a-much-longer-member-name.o".to_owned(), macho(&["_gamma"])),
            ("odd.o".to_owned(), b"xyz".to_vec()),
        ];
        for kind in [Kind::Darwin, Kind::Gnu, Kind::Coff] {
            let bytes = write(kind, &members);
            let a = parse(&bytes).unwrap();
            let names: Vec<&str> = a.members.iter().map(|m| m.name.as_str()).collect();
            assert_eq!(
                names,
                ["a.o", "a-much-longer-member-name.o", "odd.o"],
                "{kind:?}"
            );
            for (m, (_, data)) in a.members.iter().zip(&members) {
                assert!(m.data.starts_with(data), "{kind:?} {}", m.name);
                if kind == Kind::Darwin {
                    assert_eq!(m.header % 8, 0);
                    assert_eq!((m.data.as_ptr() as usize - bytes.as_ptr() as usize) % 8, 0);
                }
            }
            let syms: Vec<(&str, usize)> =
                a.symbols.iter().map(|(n, i)| (n.as_str(), *i)).collect();
            assert_eq!(
                syms,
                [("_alpha", 0), ("_beta", 0), ("_gamma", 1)],
                "{kind:?}"
            );
        }
    }
}
