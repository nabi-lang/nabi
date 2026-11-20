//! A relocatable object read back (ELF, Mach-O 64, COFF), as far as its
//! source needs: sections, their relocations, symbols.

/// The container format.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Elf,
    MachO,
    Coff,
}

pub struct Object {
    pub format: Format,
    pub sections: Vec<Section>,
    pub syms: Vec<Symbol>,
    /// ELF: RELA (the addend in the relocation).
    pub rela: bool,
}

#[derive(Default)]
pub struct Section {
    pub name: String,
    /// Mach-O: the segment.
    pub seg: String,
    /// ELF `sh_type`; Mach-O and COFF: the flags.
    pub ty: u32,
    /// ELF `sh_flags`.
    pub flags: u64,
    /// ELF `sh_link` (a section index).
    pub link: u32,
    pub entsize: u64,
    /// In bytes.
    pub align: u64,
    pub data: Vec<u8>,
    /// No contents (ELF `SHT_NOBITS`): its size.
    pub bss: Option<u64>,
    /// Mach-O: the section's address.
    pub addr: u64,
    pub relocs: Vec<Rel>,
    /// COFF: the COMDAT selection and associated section (1-based).
    pub comdat: Option<(u8, u32)>,
}

#[derive(Clone, Copy)]
pub struct Rel {
    pub off: u64,
    pub ty: u32,
    /// A symbol index; Mach-O non-extern: the section (1-based).
    pub sym: u32,
    /// ELF RELA's addend.
    pub addend: i64,
    /// Mach-O: `r_extern`, `r_length`.
    pub ext: bool,
    pub len: u8,
}

#[derive(Default, Clone)]
pub struct Symbol {
    pub name: String,
    pub value: u64,
    pub size: u64,
    /// The section index (ELF: as in the file; Mach-O and COFF: 1-based),
    /// 0 when undefined.
    pub sect: u32,
    /// ELF `st_info`, Mach-O `n_type`, COFF storage class.
    pub info: u8,
    /// ELF `st_other`, Mach-O `n_desc`, COFF type.
    pub other: u16,
    /// COFF: absolute (section -1).
    pub abs: bool,
    /// COFF: the auxiliary records.
    pub aux: Vec<u8>,
    /// COFF: a section's own symbol.
    pub section_sym: bool,
}

const fn u16_at(b: &[u8], at: usize, be: bool) -> u16 {
    let x = [b[at], b[at + 1]];
    if be {
        u16::from_be_bytes(x)
    } else {
        u16::from_le_bytes(x)
    }
}

const fn u32_at(b: &[u8], at: usize, be: bool) -> u32 {
    let x = [b[at], b[at + 1], b[at + 2], b[at + 3]];
    if be {
        u32::from_be_bytes(x)
    } else {
        u32::from_le_bytes(x)
    }
}

fn u64_at(b: &[u8], at: usize, be: bool) -> u64 {
    let lo = u64::from(u32_at(b, at, be));
    let hi = u64::from(u32_at(b, at + 4, be));
    if be { (lo << 32) | hi } else { (hi << 32) | lo }
}

fn cstr(b: &[u8], at: usize) -> String {
    let end = b[at..]
        .iter()
        .position(|&c| c == 0)
        .map_or(b.len(), |p| at + p);
    String::from_utf8_lossy(&b[at..end]).into_owned()
}

/// Read `b`, an object nabi wrote.
#[must_use]
pub fn read(b: &[u8]) -> Object {
    if b.starts_with(b"\x7fELF") {
        elf(b)
    } else if b.starts_with(&0xfeed_facf_u32.to_le_bytes()) {
        macho(b)
    } else {
        coff(b)
    }
}

fn elf(b: &[u8]) -> Object {
    let c64 = b[4] == 2;
    let be = b[5] == 2;
    let (shoff, shentsize, shnum, shstrndx) = if c64 {
        (
            u64_at(b, 0x28, be) as usize,
            u16_at(b, 0x3a, be) as usize,
            u16_at(b, 0x3c, be) as usize,
            u16_at(b, 0x3e, be) as usize,
        )
    } else {
        (
            u32_at(b, 0x20, be) as usize,
            u16_at(b, 0x2e, be) as usize,
            u16_at(b, 0x30, be) as usize,
            u16_at(b, 0x32, be) as usize,
        )
    };
    let hdr = |k: usize| shoff + k * shentsize;
    // (name, type, flags, offset, size, link, info, align, entsize)
    let field = |k: usize| {
        let h = hdr(k);
        if c64 {
            (
                u32_at(b, h, be),
                u32_at(b, h + 4, be),
                u64_at(b, h + 8, be),
                u64_at(b, h + 24, be) as usize,
                u64_at(b, h + 32, be) as usize,
                u32_at(b, h + 40, be),
                u32_at(b, h + 44, be),
                u64_at(b, h + 48, be),
                u64_at(b, h + 56, be),
            )
        } else {
            (
                u32_at(b, h, be),
                u32_at(b, h + 4, be),
                u64::from(u32_at(b, h + 8, be)),
                u32_at(b, h + 16, be) as usize,
                u32_at(b, h + 20, be) as usize,
                u32_at(b, h + 24, be),
                u32_at(b, h + 28, be),
                u64::from(u32_at(b, h + 32, be)),
                u64::from(u32_at(b, h + 36, be)),
            )
        }
    };
    let shnum = if shnum == 0 { field(0).4 } else { shnum };
    let shstrndx = if shstrndx == 0xffff {
        field(0).5 as usize
    } else {
        shstrndx
    };
    let strs = field(shstrndx);
    let mut sections = Vec::with_capacity(shnum);
    let mut rela = false;
    let mut symtab = None;
    let mut shndx = None;
    for k in 0..shnum {
        let (name, ty, flags, off, size, link, info, align, entsize) = field(k);
        let name = if k == 0 {
            String::new()
        } else {
            cstr(b, strs.3 + name as usize)
        };
        match ty {
            2 => symtab = Some((off, size, link)),
            18 => shndx = Some(off),
            4 => rela = true,
            _ => {}
        }
        let _ = info;
        sections.push(Section {
            name,
            ty,
            flags,
            link,
            entsize,
            align,
            data: if ty == 8 {
                Vec::new()
            } else {
                b[off..off + size].to_vec()
            },
            bss: (ty == 8).then_some(size as u64),
            ..Section::default()
        });
    }
    let mut syms = Vec::new();
    if let Some((off, size, link)) = symtab {
        let strtab = field(link as usize).3;
        let ent = if c64 { 24 } else { 16 };
        for (n, at) in (off..off + size).step_by(ent).enumerate() {
            let (name, value, sz, info, other, sect) = if c64 {
                (
                    u32_at(b, at, be),
                    u64_at(b, at + 8, be),
                    u64_at(b, at + 16, be),
                    b[at + 4],
                    b[at + 5],
                    u16_at(b, at + 6, be),
                )
            } else {
                (
                    u32_at(b, at, be),
                    u64::from(u32_at(b, at + 4, be)),
                    u64::from(u32_at(b, at + 8, be)),
                    b[at + 12],
                    b[at + 13],
                    u16_at(b, at + 14, be),
                )
            };
            let sect = match (sect, shndx) {
                (0xffff, Some(x)) => u32_at(b, x + 4 * n, be),
                (s, _) => u32::from(s),
            };
            syms.push(Symbol {
                name: cstr(b, strtab + name as usize),
                value,
                size: sz,
                sect,
                info,
                other: u16::from(other),
                ..Symbol::default()
            });
        }
    }
    for k in 0..shnum {
        let (_, ty, _, off, size, _, info, _, _) = field(k);
        if ty != 4 && ty != 9 {
            continue;
        }
        let ent = match (c64, ty == 4) {
            (true, true) => 24,
            (true, false) => 16,
            (false, true) => 12,
            (false, false) => 8,
        };
        let mut rs = Vec::new();
        for at in (off..off + size).step_by(ent) {
            let (o, sym, rty, addend) = if c64 {
                let i = u64_at(b, at + 8, be);
                let a = if ty == 4 {
                    u64_at(b, at + 16, be) as i64
                } else {
                    0
                };
                (u64_at(b, at, be), (i >> 32) as u32, i as u32, a)
            } else {
                let i = u32_at(b, at + 4, be);
                let a = if ty == 4 {
                    i64::from(u32_at(b, at + 8, be) as i32)
                } else {
                    0
                };
                (u64::from(u32_at(b, at, be)), i >> 8, i & 0xff, a)
            };
            rs.push(Rel {
                off: o,
                ty: rty,
                sym,
                addend,
                ext: true,
                len: 0,
            });
        }
        sections[info as usize].relocs = rs;
    }
    Object {
        format: Format::Elf,
        sections,
        syms,
        rela,
    }
}

fn macho(b: &[u8]) -> Object {
    let ncmds = u32_at(b, 16, false) as usize;
    let mut at = 32;
    let mut sections = Vec::new();
    let mut syms = Vec::new();
    for _ in 0..ncmds {
        let cmd = u32_at(b, at, false);
        let size = u32_at(b, at + 4, false) as usize;
        if cmd == 0x19 {
            let n = u32_at(b, at + 64, false) as usize;
            for k in 0..n {
                let s = at + 72 + 80 * k;
                let name = cstr(&b[s..s + 16], 0);
                let seg = cstr(&b[s + 16..s + 32], 0);
                let addr = u64_at(b, s + 32, false);
                let sz = u64_at(b, s + 40, false) as usize;
                let off = u32_at(b, s + 48, false) as usize;
                let align = u32_at(b, s + 52, false);
                let reloff = u32_at(b, s + 56, false) as usize;
                let nreloc = u32_at(b, s + 60, false) as usize;
                let flags = u32_at(b, s + 64, false);
                let relocs = (0..nreloc)
                    .map(|r| {
                        let a = reloff + 8 * r;
                        let i = u32_at(b, a + 4, false);
                        Rel {
                            off: u64::from(u32_at(b, a, false)),
                            ty: i >> 28,
                            sym: i & 0x00ff_ffff,
                            addend: 0,
                            ext: i & (1 << 27) != 0,
                            len: ((i >> 25) & 3) as u8,
                        }
                    })
                    .collect();
                sections.push(Section {
                    name,
                    seg,
                    ty: flags,
                    align: 1 << align,
                    data: b[off..off + sz].to_vec(),
                    addr,
                    relocs,
                    ..Section::default()
                });
            }
        } else if cmd == 0x2 {
            let symoff = u32_at(b, at + 8, false) as usize;
            let nsyms = u32_at(b, at + 12, false) as usize;
            let stroff = u32_at(b, at + 16, false) as usize;
            for k in 0..nsyms {
                let s = symoff + 16 * k;
                syms.push(Symbol {
                    name: cstr(b, stroff + u32_at(b, s, false) as usize),
                    info: b[s + 4],
                    sect: u32::from(b[s + 5]),
                    other: u16_at(b, s + 6, false),
                    value: u64_at(b, s + 8, false),
                    ..Symbol::default()
                });
            }
        }
        at += size;
    }
    Object {
        format: Format::MachO,
        sections,
        syms,
        rela: false,
    }
}

fn coff(b: &[u8]) -> Object {
    let big = u16_at(b, 0, false) == 0 && u16_at(b, 2, false) == 0xffff;
    let (nsects, symoff, nsyms, hdrs, ent) = if big {
        (
            u32_at(b, 44, false) as usize,
            u32_at(b, 48, false) as usize,
            u32_at(b, 52, false) as usize,
            56,
            20,
        )
    } else {
        (
            u16_at(b, 2, false) as usize,
            u32_at(b, 8, false) as usize,
            u32_at(b, 12, false) as usize,
            20 + u16_at(b, 16, false) as usize,
            18,
        )
    };
    let strtab = symoff + ent * nsyms;
    let name8 = |n: &[u8]| -> String {
        if n[0] == b'/' {
            let off: usize = String::from_utf8_lossy(&n[1..])
                .trim_end_matches('\0')
                .parse()
                .unwrap_or(0);
            cstr(b, strtab + off)
        } else {
            cstr(n, 0)
        }
    };
    let mut sections = Vec::with_capacity(nsects);
    for k in 0..nsects {
        let h = hdrs + 40 * k;
        let size = u32_at(b, h + 16, false) as usize;
        let data = u32_at(b, h + 20, false) as usize;
        let reloff = u32_at(b, h + 24, false) as usize;
        let flags = u32_at(b, h + 36, false);
        // More than 0xffff: the count is in a first, extra record.
        let (first, nreloc) = if flags & 0x0100_0000 != 0 {
            (1, u32_at(b, reloff, false) as usize)
        } else {
            (0, u16_at(b, h + 32, false) as usize)
        };
        let relocs = (first..nreloc)
            .map(|r| {
                let a = reloff + 10 * r;
                Rel {
                    off: u64::from(u32_at(b, a, false)),
                    sym: u32_at(b, a + 4, false),
                    ty: u32::from(u16_at(b, a + 8, false)),
                    addend: 0,
                    ext: true,
                    len: 0,
                }
            })
            .collect();
        let align = (flags >> 20) & 0xf;
        sections.push(Section {
            name: name8(&b[h..h + 8]),
            ty: flags,
            align: if align == 0 { 1 } else { 1 << (align - 1) },
            data: if data == 0 {
                vec![0; size]
            } else {
                b[data..data + size].to_vec()
            },
            relocs,
            ..Section::default()
        });
    }
    let mut syms = Vec::with_capacity(nsyms);
    let mut k = 0;
    while k < nsyms {
        let s = symoff + ent * k;
        let name = if u32_at(b, s, false) == 0 {
            cstr(b, strtab + u32_at(b, s + 4, false) as usize)
        } else {
            cstr(&b[s..s + 8], 0)
        };
        let (sect, rest) = if big {
            (u32_at(b, s + 12, false) as i32, s + 16)
        } else {
            (i32::from(u16_at(b, s + 12, false) as i16), s + 14)
        };
        let naux = b[rest + 3] as usize;
        let aux = b[s + ent..s + ent * (1 + naux)].to_vec();
        let mut sym = Symbol {
            name,
            value: u64::from(u32_at(b, s + 8, false)),
            sect: sect.max(0) as u32,
            abs: sect == -1,
            other: u16_at(b, rest, false),
            info: b[rest + 2],
            aux,
            ..Symbol::default()
        };
        // A section's definition: static, value 0, one auxiliary record.
        if sym.info == 3 && naux == 1 && sym.value == 0 && sym.sect > 0 {
            let sec = &mut sections[sym.sect as usize - 1];
            if sec.name == sym.name {
                sym.section_sym = true;
                let a = &sym.aux;
                let sel = a[14];
                let assoc =
                    u32::from(u16_at(a, 12, false)) | (u32::from(u16_at(a, 16, false)) << 16);
                if sec.ty & 0x1000 != 0 {
                    sec.comdat = Some((sel, assoc));
                }
            }
        }
        syms.push(sym);
        // Auxiliary records take the indices after their symbol.
        for _ in 0..naux {
            syms.push(Symbol::default());
        }
        k += 1 + naux;
    }
    Object {
        format: Format::Coff,
        sections,
        syms,
        rela: false,
    }
}
